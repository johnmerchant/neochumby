//! AirPlay's Chumby side, as far as it needs no Chumby, so it's tested on
//! the host: the service's memory of what's playing, which it hands its
//! frontends ([`hub`]), and AirPlay's own frontend, the now-playing screen
//! in text mode ([`screen`]). The messages are neochumby_airplay_proto's.

pub mod hub;
pub mod screen;
