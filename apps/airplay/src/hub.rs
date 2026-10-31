//! What the service knows of what's playing, for its frontends: the latest
//! of each thing the receiver has said, so that a frontend that connects
//! late is told it all at once, then each change as it comes.

use neochumby_airplay_proto::frontend::ToFrontend;
use neochumby_airplay_proto::receiver::ToChumby;
use neochumby_airplay_proto::{Cover, Play, State, Track, VERSION};

#[derive(Default)]
pub struct Hub {
    link: bool,
    name: String,
    track: Track,
    cover: Cover,
    state: State,
}

impl Hub {
    /// Everything known, for a frontend that has just connected: Hello
    /// first.
    pub fn snapshot(&self) -> Vec<ToFrontend> {
        vec![
            ToFrontend::Hello { version: VERSION },
            ToFrontend::Link(self.link),
            ToFrontend::Name(self.name.clone()),
            ToFrontend::Track(self.track.clone()),
            ToFrontend::Cover(self.cover.clone()),
            ToFrontend::State(self.state),
        ]
    }

    /// The receiver has come or gone: what to tell the frontends. With it
    /// gone, nothing plays, and nothing is known of what did.
    pub fn link(&mut self, up: bool) -> Vec<ToFrontend> {
        if up == self.link {
            return Vec::new();
        }
        self.link = up;
        let mut out = vec![ToFrontend::Link(up)];
        if !up {
            *self = Hub::default();
            out.extend([
                ToFrontend::Track(Track::default()),
                ToFrontend::Cover(Cover::default()),
                ToFrontend::State(State::default()),
            ]);
        }
        out
    }

    /// Takes in something the receiver sent, other than audio: what to
    /// tell the frontends, and whether a stream has just begun (it was
    /// stopped, and plays), which opens the screen. A resume from a pause
    /// doesn't.
    pub fn receive(&mut self, m: &ToChumby) -> (Option<ToFrontend>, bool) {
        match m {
            ToChumby::Name(n) => {
                self.name = n.clone();
                (Some(ToFrontend::Name(n.clone())), false)
            }
            ToChumby::Track(t) => {
                self.track = t.clone();
                (Some(ToFrontend::Track(t.clone())), false)
            }
            ToChumby::Cover(c) => {
                self.cover = c.clone();
                (Some(ToFrontend::Cover(c.clone())), false)
            }
            ToChumby::State(s) => {
                let began = self.state.play == Play::Stopped && s.play == Play::Playing;
                self.state = *s;
                (Some(ToFrontend::State(*s)), began)
            }
            ToChumby::Audio(_) | ToChumby::End => (None, false),
        }
    }

    /// Whether the sender is playing, as it last said.
    pub fn play(&self) -> Play {
        self.state.play
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(play: Play) -> ToChumby {
        ToChumby::State(State { play, ..State::default() })
    }

    #[test]
    fn a_new_frontend_is_told_everything() {
        let mut h = Hub::default();
        h.link(true);
        let track = Track { title: "Song".into(), ..Track::default() };
        h.receive(&ToChumby::Name("Den".into()));
        h.receive(&ToChumby::Track(track.clone()));
        let s = h.snapshot();
        assert_eq!(s[0], ToFrontend::Hello { version: VERSION });
        assert_eq!(s[1], ToFrontend::Link(true));
        assert_eq!(s[2], ToFrontend::Name("Den".into()));
        assert_eq!(s[3], ToFrontend::Track(track));
    }

    #[test]
    fn only_a_stream_beginning_opens_the_screen() {
        let mut h = Hub::default();
        assert!(h.receive(&state(Play::Playing)).1, "stopped, then playing");
        assert!(!h.receive(&state(Play::Playing)).1);
        assert!(!h.receive(&state(Play::Paused)).1);
        assert!(!h.receive(&state(Play::Playing)).1, "a resume");
        h.receive(&state(Play::Stopped));
        assert!(h.receive(&state(Play::Playing)).1, "the next stream");
        assert_eq!(h.receive(&ToChumby::End), (None, false));
    }

    #[test]
    fn with_the_receiver_gone_nothing_is_known() {
        let mut h = Hub::default();
        assert_eq!(h.link(true), [ToFrontend::Link(true)]);
        assert!(h.link(true).is_empty());
        h.receive(&ToChumby::Track(Track { title: "Song".into(), ..Track::default() }));
        h.receive(&state(Play::Playing));
        let gone = h.link(false);
        assert_eq!(gone[0], ToFrontend::Link(false));
        assert!(gone.contains(&ToFrontend::State(State::default())));
        assert_eq!(h.play(), Play::Stopped);
        assert_eq!(h.snapshot()[3], ToFrontend::Track(Track::default()));
    }
}
