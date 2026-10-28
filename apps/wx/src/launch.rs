//! Opening another app from the tree (the `app NAME` action). Kept behind
//! one small trait because how a home screen opens apps is still being
//! decided (johnmerchant/neochumby#8): today the home screen runs the app
//! itself and waits, as neochumby-home does; a session pipe that opens it
//! for us, while we stay resident and don't draw, would be another
//! [`Launcher`].

/// What opening an app did.
#[derive(Debug, PartialEq)]
pub enum Opened {
    /// It ran and has exited: the screen is ours again.
    Ran,
    /// It is in front now; we are in the background until
    /// [`Launcher::returned`] says otherwise. We don't draw meanwhile, and
    /// hold as little memory as we can. (For a session that opens apps for
    /// us; nothing returns it yet.)
    #[allow(dead_code)]
    InFront,
    /// There's no such app here (or it can't run).
    #[cfg_attr(not(target_arch = "arm"), allow(dead_code))]
    Missing,
}

pub trait Launcher {
    /// Whether the app gets the inputs to itself (the top button's device
    /// is opened exclusively), so we let go of them first.
    fn takes_inputs(&self) -> bool;
    fn open(&mut self, name: &str) -> Opened;
    /// After [`Opened::InFront`]: whether we are in front again.
    fn returned(&mut self) -> bool {
        true
    }
}

/// On the host: nothing to run; it reports what it would do.
pub struct Pretend;

impl Launcher for Pretend {
    fn takes_inputs(&self) -> bool {
        false
    }

    fn open(&mut self, name: &str) -> Opened {
        eprintln!("[app] {name}: not run on the host");
        Opened::Ran
    }
}

/// Runs apps/<name>/'s exec as neochumby-home does: in its directory, with
/// NEOCHUMBY_ROOT and HOME=/tmp, waiting until it exits.
#[cfg(target_arch = "arm")]
pub struct DirectExec;

#[cfg(target_arch = "arm")]
impl Launcher for DirectExec {
    fn takes_inputs(&self) -> bool {
        true
    }

    fn open(&mut self, name: &str) -> Opened {
        use neochumby::apps;
        let root = apps::root();
        let Some(app) = apps::all(&root).into_iter().find(|a| a.name == name) else { return Opened::Missing };
        let Some(exec) = app.exec.clone().filter(|_| app.runnable()) else { return Opened::Missing };
        let status = std::process::Command::new(&exec)
            .current_dir(&app.dir)
            .env("NEOCHUMBY_ROOT", &root)
            .env("HOME", "/tmp")
            .status();
        eprintln!("neochumby-wx: {name} exited: {status:?}");
        Opened::Ran
    }
}
