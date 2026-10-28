/*
 * Doom's sound module (i_sound.h), with doomgeneric built for
 * FEATURE_SOUND: the effects' lumps are found and their DMX headers read
 * here, and src/sound.rs mixes them onto the speaker. Music has no player
 * yet: DG_music_module takes the songs and plays nothing.
 */

#include <stdlib.h>
#include <string.h>

#include "deh_str.h"
#include "i_sound.h"
#include "m_misc.h"
#include "w_wad.h"
#include "z_zone.h"

/* m_config.c binds these with FEATURE_SOUND; there is no libsamplerate. */
int use_libsamplerate = 0;
float libsamplerate_scale = 0.65f;

void chum_sound_init (void);
void chum_sound_shutdown (void);
void chum_sound_update (void);
void chum_sound_start (int channel, const byte *samples, int length, int rate, int vol, int sep);
void chum_sound_params (int channel, int vol, int sep);
void chum_sound_stop (int channel);
int chum_sound_playing (int channel);

/* An effect's 8-bit unsigned samples, kept for the life of the process. */
typedef struct
{
	int	rate;
	int	length;
	byte	samples[];
} effect_t;

static boolean	use_prefix;

static boolean Init (boolean use_sfx_prefix)
{
	use_prefix = use_sfx_prefix;
	chum_sound_init ();
	return true;
}

static int GetSfxLumpNum (sfxinfo_t *sfx)
{
	char	name[9];

	if (sfx->link)
		sfx = sfx->link;
	if (use_prefix)
		M_snprintf (name, sizeof name, "ds%s", DEH_String (sfx->name));
	else
		M_StringCopy (name, DEH_String (sfx->name), sizeof name);
	return W_GetNumForName (name);
}

/*
 * As Chocolate Doom reads a DMX lump: format 3, a 16-bit rate and a 32-bit
 * length, then the samples, of which DMX skips the first and last 16.
 * NULL for a lump that isn't one, as DMX plays nothing for it.
 */
static effect_t *Load (sfxinfo_t *sfx)
{
	effect_t	*e;
	byte	*data;
	unsigned int	lumplen, length;

	if (sfx->driver_data)
		return sfx->driver_data;
	data = W_CacheLumpNum (sfx->lumpnum, PU_STATIC);
	lumplen = W_LumpLength (sfx->lumpnum);
	e = NULL;
	if (lumplen >= 8 && data[0] == 3 && data[1] == 0)
	{
		length = data[4] | (data[5] << 8) | (data[6] << 16) | ((unsigned int)data[7] << 24);
		if (length <= lumplen - 8 && length > 48)
		{
			e = malloc (sizeof *e + length - 32);
			if (e)
			{
				e->rate = data[2] | (data[3] << 8);
				e->length = length - 32;
				memcpy (e->samples, data + 8 + 16, e->length);
			}
		}
	}
	W_ReleaseLumpNum (sfx->lumpnum);
	sfx->driver_data = e;
	return e;
}

static int StartSound (sfxinfo_t *sfx, int channel, int vol, int sep)
{
	effect_t	*e = Load (sfx);

	if (!e)
		return -1;
	chum_sound_start (channel, e->samples, e->length, e->rate, vol, sep);
	return channel;
}

static boolean SoundIsPlaying (int channel)
{
	return chum_sound_playing (channel) != 0;
}

static void CacheSounds (sfxinfo_t *sounds, int num_sounds)
{
}

static snddevice_t	sound_devices[] = { SNDDEVICE_SB, SNDDEVICE_PAS, SNDDEVICE_GUS, SNDDEVICE_WAVEBLASTER, SNDDEVICE_SOUNDCANVAS, SNDDEVICE_AWE32 };

sound_module_t DG_sound_module =
{
	sound_devices,
	sizeof sound_devices / sizeof *sound_devices,
	Init,
	chum_sound_shutdown,
	GetSfxLumpNum,
	chum_sound_update,
	chum_sound_params,
	StartSound,
	chum_sound_stop,
	SoundIsPlaying,
	CacheSounds,
};

/* ---- music: none yet ------------------------------------------------ */

static boolean MusInit (void)
{
	return true;
}

static void MusNothing (void)
{
}

static void MusVolume (int volume)
{
}

static void *MusRegister (void *data, int len)
{
	return NULL;
}

static void MusUnregister (void *handle)
{
}

static void MusPlay (void *handle, boolean looping)
{
}

static boolean MusPlaying (void)
{
	return false;
}

music_module_t DG_music_module =
{
	sound_devices,
	sizeof sound_devices / sizeof *sound_devices,
	MusInit,
	MusNothing,
	MusVolume,
	MusNothing,
	MusNothing,
	MusRegister,
	MusUnregister,
	MusPlay,
	MusNothing,
	MusPlaying,
	NULL,
};
