/*
 * The two platform functions the Quake engine calls with a format and
 * arguments. They are the only C of neochumby-quake's own: stable Rust can
 * call a variadic function but not define one. Each formats its message
 * and hands it to src/sys.rs.
 *
 * Copyright (C) 1996-1997 Id Software, Inc. (as sys_null.c);
 * neochumby-quake contributors. GPL-2.0-or-later, as the engine.
 */

#include <stdarg.h>
#include <stdio.h>

void chum_log (const char *msg);
void chum_error (const char *msg);

void Sys_Error (char *error, ...)
{
	va_list	argptr;
	char	msg[1024];

	va_start (argptr, error);
	vsnprintf (msg, sizeof msg, error, argptr);
	va_end (argptr);
	chum_error (msg);
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
