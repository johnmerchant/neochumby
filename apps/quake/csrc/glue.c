/*
 * neochumby-quake's glue between the Quake engine and the Chumby platform layer
 * (src/, Rust). Only what has to be C is here: the engine's globals and
 * structs, its varargs entry points, and the main loop. The file functions
 * follow id's sys_null.c.
 *
 * Copyright (C) 1996-1997 Id Software, Inc. (the parts from sys_null.c and
 * vid_null.c); neochumby-quake contributors. GPL-2.0-or-later, as the engine.
 */

#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <netdb.h>
#include <netinet/in.h>
#include <unistd.h>

#include "quakedef.h"
#include "d_local.h"

/* The platform layer, src/*.rs. */
void chum_fb_init (void);
void chum_fb_palette (const unsigned char *palette);
void chum_fb_present (const unsigned char *buf, int width, int height, int rowbytes);
double chum_time (void);
int chum_poll_key (int *key, int *down);
void chum_log (const char *msg);

/* ---- video ---------------------------------------------------------- */

#define WIDTH	320
#define HEIGHT	240

viddef_t	vid;
unsigned short	d_8to16table[256];
unsigned	d_8to24table[256];

static byte	vid_buffer[WIDTH * HEIGHT];
static short	zbuffer[WIDTH * HEIGHT];

void VID_SetPalette (unsigned char *palette)
{
	chum_fb_palette (palette);
}

void VID_ShiftPalette (unsigned char *palette)
{
	chum_fb_palette (palette);
}

void VID_Init (unsigned char *palette)
{
	int	cachesize;
	void	*cache;

	vid.maxwarpwidth = vid.width = vid.conwidth = WIDTH;
	vid.maxwarpheight = vid.height = vid.conheight = HEIGHT;
	/* square pixels on a 320x240 panel */
	vid.aspect = ((float)vid.height / (float)vid.width) * (320.0 / 240.0);
	vid.numpages = 1;
	vid.colormap = host_colormap;
	vid.fullbright = 256 - LittleLong (*((int *)vid.colormap + 2048));
	vid.buffer = vid.conbuffer = vid_buffer;
	vid.rowbytes = vid.conrowbytes = WIDTH;

	d_pzbuffer = zbuffer;
	cachesize = D_SurfaceCacheForRes (WIDTH, HEIGHT);
	cache = malloc (cachesize);
	if (!cache)
		Sys_Error ("VID_Init: no memory for the surface cache");
	D_InitCaches (cache, cachesize);

	chum_fb_init ();
	chum_fb_palette (palette);
}

void VID_Shutdown (void)
{
}

void VID_Update (vrect_t *rects)
{
	chum_fb_present (vid.buffer, vid.width, vid.height, vid.rowbytes);
}

void D_BeginDirectRect (int x, int y, byte *pbitmap, int width, int height)
{
}

void D_EndDirectRect (int x, int y, int width, int height)
{
}

/* ---- input ---------------------------------------------------------- */

void IN_Init (void)
{
}

void IN_Shutdown (void)
{
}

void IN_Commands (void)
{
}

void IN_Move (usercmd_t *cmd)
{
}

void Sys_SendKeyEvents (void)
{
	int	key, down;

	while (chum_poll_key (&key, &down))
		Key_Event (key, down);
}

/* ---- files (as sys_null.c) ------------------------------------------ */

#define MAX_HANDLES	10
static FILE	*sys_handles[MAX_HANDLES];

static int findhandle (void)
{
	int	i;

	for (i = 1; i < MAX_HANDLES; i++)
		if (!sys_handles[i])
			return i;
	Sys_Error ("out of handles");
	return -1;
}

static int filelength (FILE *f)
{
	int	pos, end;

	pos = ftell (f);
	fseek (f, 0, SEEK_END);
	end = ftell (f);
	fseek (f, pos, SEEK_SET);
	return end;
}

int Sys_FileOpenRead (char *path, int *hndl)
{
	FILE	*f;
	int	i;

	i = findhandle ();
	f = fopen (path, "rb");
	if (!f)
	{
		*hndl = -1;
		return -1;
	}
	sys_handles[i] = f;
	*hndl = i;
	return filelength (f);
}

int Sys_FileOpenWrite (char *path)
{
	FILE	*f;
	int	i;

	i = findhandle ();
	f = fopen (path, "wb");
	if (!f)
		Sys_Error ("Error opening %s: %s", path, strerror (errno));
	sys_handles[i] = f;
	return i;
}

void Sys_FileClose (int handle)
{
	fclose (sys_handles[handle]);
	sys_handles[handle] = NULL;
}

void Sys_FileSeek (int handle, int position)
{
	fseek (sys_handles[handle], position, SEEK_SET);
}

int Sys_FileRead (int handle, void *dest, int count)
{
	return fread (dest, 1, count, sys_handles[handle]);
}

int Sys_FileWrite (int handle, void *data, int count)
{
	return fwrite (data, 1, count, sys_handles[handle]);
}

int Sys_FileTime (char *path)
{
	struct stat	st;

	return stat (path, &st) == 0 ? (int)st.st_mtime : -1;
}

void Sys_mkdir (char *path)
{
	mkdir (path, 0777);
}

/* ---- the rest of sys ------------------------------------------------ */

/* sys_linux.c's: the Chumby is never the dedicated server. */
qboolean	isDedicated;

void Sys_MakeCodeWriteable (unsigned long startaddr, unsigned long length)
{
}

void Sys_Error (char *error, ...)
{
	va_list	argptr;
	char	msg[1024];

	va_start (argptr, error);
	vsnprintf (msg, sizeof msg, error, argptr);
	va_end (argptr);
	chum_log ("Sys_Error: ");
	chum_log (msg);
	chum_log ("\n");
	exit (1);
}

void Sys_Printf (char *fmt, ...)
{
	va_list	argptr;
	char	msg[1024];

	va_start (argptr, fmt);
	vsnprintf (msg, sizeof msg, fmt, argptr);
	va_end (argptr);
	chum_log (msg);
}

void Sys_Quit (void)
{
	Host_Shutdown ();
	exit (0);
}

double Sys_FloatTime (void)
{
	return chum_time ();
}

char *Sys_ConsoleInput (void)
{
	return NULL;
}

void Sys_Sleep (void)
{
}

void Sys_HighFPPrecision (void)
{
}

void Sys_LowFPPrecision (void)
{
}

/* ---- networking ----------------------------------------------------- */

/*
 * net_udp.c is built with gethostbyname renamed to this (build.rs). It
 * looks up its own address from its host name, and crashed when that
 * failed, as it does for a Chumby whose name isn't in DNS: this host's own
 * name gives INADDR_ANY then, which will do. Any other name is looked up
 * as before.
 */
struct hostent *chum_gethostbyname (const char *name)
{
	static in_addr_t	any = INADDR_ANY;
	static char	*addrs[] = { (char *)&any, NULL };
	static struct hostent	self = { NULL, NULL, AF_INET, sizeof any, addrs };
	struct hostent	*h = gethostbyname (name);
	char	me[256];

	if (h || gethostname (me, sizeof me) != 0 || strcmp (me, name) != 0)
		return h;
	self.h_name = (char *)name;
	return &self;
}

/* ---- main loop ------------------------------------------------------ */

int chum_quake_main (int argc, char **argv)
{
	static quakeparms_t	parms;
	double	oldtime, newtime;
	int	p;

	COM_InitArgv (argc, argv);
	parms.argc = com_argc;
	parms.argv = com_argv;

	/* Most of the RAM left once the stock UI is gone; -mem <MB> overrides. */
	parms.memsize = 10 * 1024 * 1024;
	p = COM_CheckParm ("-mem");
	if (p && p + 1 < com_argc)
		parms.memsize = (int)(Q_atof (com_argv[p + 1]) * 1024 * 1024);
	parms.membase = malloc (parms.memsize);
	if (!parms.membase)
		Sys_Error ("no memory for a %d-byte heap", parms.memsize);
	parms.basedir = ".";

	Host_Init (&parms);

	oldtime = Sys_FloatTime ();
	while (1)
	{
		newtime = Sys_FloatTime ();
		Host_Frame (newtime - oldtime);
		oldtime = newtime;
	}
	return 0;
}
