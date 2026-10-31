//! GET APPS's screens and what each tap does: [`App`] holds what the
//! server has said and which screen is up, draws it as a [`Frame`], and
//! turns taps and answers into the next screen and what to ask
//! ([`Effect`]). Nothing here touches a device, so all of it is tested on
//! the host.
//!
//! The screens: the catalog, a page at a time (title, publisher, a line of
//! description, downloads, and verified or unverified); the catalog apps
//! installed; an app's details (what it asks for, its version and size,
//! and whether it would be slow); the owner's approval; the wait for the
//! server's; progress; and where the new app is in home.

use neochumby_proto::catalog::{Answer, Ask, Details, Installed, Listing, NEVER, PAGE};

use crate::screen::{Color, Frame, Rect, Style, wrap};

/// What a tap can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Prev,
    Next,
    /// The catalog apps installed.
    ShowInstalled,
    /// The catalog.
    ShowCatalog,
    Exit,
    /// The catalog's app at this place in the whole.
    Pick(usize),
    /// The installed app at this place in the list.
    PickInstalled(usize),
    Back,
    /// Install, or update to, the app shown: to the approval.
    Get,
    /// The owner allows the app shown what it asks.
    Allow,
    /// Remove the app shown: to the approval.
    Remove,
    /// The owner allows the removal.
    AllowRemove,
    Cancel,
    Retry,
}

/// What the device loop does for the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Send(Ask),
    /// Open the companion again.
    Connect,
    Exit,
}

/// The connection to neochumbyd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conn {
    Connecting,
    Up,
    Down(String),
}

/// Where Back goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    Catalog,
    Installed,
}

/// The screen up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Catalog,
    Installed,
    /// Waiting for the server: what it is doing.
    Busy {
        what: String,
        from: From,
    },
    Details {
        d: Box<Details>,
        from: From,
    },
    /// The owner's approval of an install or update, on the Chumby.
    Approve {
        d: Box<Details>,
        from: From,
    },
    /// The owner's approval of a removal.
    ApproveRemove {
        app: String,
        title: String,
        from: From,
    },
    /// The request waits for the owner on the server.
    Waiting {
        code: String,
        minutes: u16,
        title: String,
        remove: bool,
    },
    Progress {
        title: String,
        steps: Vec<String>,
    },
    Done {
        title: String,
        place: Place,
        /// Whether the session starts again once GET APPS is left; else
        /// the next sync brings the app.
        restarts: bool,
    },
    Removed {
        title: String,
    },
    Failed {
        why: String,
        from: From,
    },
}

/// Where home lists an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub page: usize,
    pub pages: usize,
    pub row: usize,
}

/// Home's rows on a page (apps/home: ROWS): with more apps than that,
/// each page has one row fewer, for MORE.
const HOME_ROWS: usize = 4;

/// Where home lists `app`, given the apps it lists in its order.
pub fn home_place(listed: &[String], app: &str) -> Option<Place> {
    let i = listed.iter().position(|a| a == app)?;
    if listed.len() <= HOME_ROWS {
        return Some(Place { page: 1, pages: 1, row: i + 1 });
    }
    let per = HOME_ROWS - 1;
    Some(Place { page: i / per + 1, pages: listed.len().div_ceil(per), row: i % per + 1 })
}

/// The catalog as far as it has been sent.
#[derive(Default)]
struct Cache {
    fetched_at: u64,
    /// None until the first page comes.
    total: Option<usize>,
    /// By place in the whole.
    apps: Vec<Option<Listing>>,
    installed: Vec<Installed>,
    compiles: bool,
    /// The pages asked for and not yet come (their `from`).
    asked: Vec<u32>,
}

/// GET APPS.
pub struct App {
    pub conn: Conn,
    pub view: View,
    cache: Cache,
    page: usize,
    installed_page: usize,
    /// The target held down, lit until let go.
    pub pressed: Option<Action>,
    /// Counts up while something is awaited, for the spinner.
    pub tick: usize,
}

const HEAD: Style = Style::fg(Color::White).on(Color::Blue).bold();
const BUTTON: Style = Style::fg(Color::Black).on(Color::Cyan);
const GO: Style = Style::fg(Color::Black).on(Color::Green);
const STOP: Style = Style::fg(Color::White).on(Color::Red).bold();
const DIM_BUTTON: Style = Style::fg(Color::Black).on(Color::Black).bold();
const LIT: Style = Style::fg(Color::Black).on(Color::White);
const TITLE: Style = Style::fg(Color::White).bold();
const TEXT: Style = Style::PLAIN;
const DIM: Style = Style::fg(Color::Black).bold();
const VERIFIED: Style = Style::fg(Color::Green).bold();
const UNVERIFIED: Style = Style::fg(Color::Yellow).bold();
const WARN: Style = Style::fg(Color::Yellow).bold();
const BAD: Style = Style::fg(Color::Red).bold();
const CODE: Style = Style::fg(Color::Cyan).bold();

/// Rows each listing takes: three of text and one between.
const ENTRY: usize = 4;
/// The button bar's height, at the bottom.
const BAR: usize = 3;

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

impl App {
    pub fn new() -> App {
        App {
            conn: Conn::Connecting,
            view: View::Catalog,
            cache: Cache::default(),
            page: 0,
            installed_page: 0,
            pressed: None,
            tick: 0,
        }
    }

    /// The companion is open: the first page.
    pub fn connected(&mut self) -> Vec<Effect> {
        self.conn = Conn::Up;
        self.cache = Cache::default();
        self.ask_page(0)
    }

    /// The companion couldn't be opened, or has gone.
    pub fn gone(&mut self, why: &str) {
        self.conn = Conn::Down(why.to_string());
        self.view = View::Catalog;
    }

    /// Whether anything on screen moves while nothing happens.
    pub fn busy(&self) -> bool {
        matches!(self.view, View::Busy { .. } | View::Waiting { .. } | View::Progress { .. })
            || self.conn == Conn::Connecting
            || (self.view == View::Catalog && self.conn == Conn::Up && self.cache.total.is_none())
    }

    fn ask_page(&mut self, from: u32) -> Vec<Effect> {
        if self.cache.asked.contains(&from) {
            return Vec::new();
        }
        self.cache.asked.push(from);
        vec![Effect::Send(Ask::List { from })]
    }

    /// Listings per catalog page, and installed apps per page, on a
    /// screen `rows` high.
    fn per_page(rows: usize) -> usize {
        (rows.saturating_sub(1 + BAR) / ENTRY).max(1)
    }

    /// Asks for the listings the page shown needs, if they haven't come.
    fn fill(&mut self, rows: usize) -> Vec<Effect> {
        let Some(total) = self.cache.total else { return Vec::new() };
        let per = Self::per_page(rows);
        let mut out = Vec::new();
        for i in self.page * per..((self.page + 1) * per).min(total) {
            if self.cache.apps.get(i).is_none_or(|a| a.is_none()) {
                out.extend(self.ask_page((i as u32 / PAGE) * PAGE));
            }
        }
        out
    }

    /// One of the server's answers.
    pub fn answer(&mut self, a: Answer, rows: usize) -> Vec<Effect> {
        match a {
            Answer::Page(p) => {
                self.cache.asked.retain(|&f| f != p.from);
                if p.from == 0 {
                    self.cache.fetched_at = p.fetched_at;
                    self.cache.installed = p.installed;
                    self.cache.compiles = p.compiles;
                }
                let total = p.total as usize;
                self.cache.total = Some(total);
                self.cache.apps.resize(total, None);
                for (i, l) in p.apps.into_iter().enumerate() {
                    if let Some(slot) = self.cache.apps.get_mut(p.from as usize + i) {
                        *slot = Some(l);
                    }
                }
                let last = Self::pages(total, Self::per_page(rows)) - 1;
                self.page = self.page.min(last);
                return self.fill(rows);
            }
            Answer::Details(d) => {
                if let View::Busy { from, .. } = self.view {
                    self.view = View::Details { d, from };
                }
            }
            Answer::Waiting { code, minutes } => {
                // A request made after its owner had cancelled it on the
                // Chumby is taken back.
                let View::Busy { what: title, .. } = &self.view else {
                    return vec![Effect::Send(Ask::Cancel)];
                };
                let (title, remove) = (title.clone(), title.starts_with("Removing"));
                let title = title.split_once(' ').map(|(_, t)| t.to_string()).unwrap_or(title);
                self.view = View::Waiting { code, minutes, title, remove };
            }
            Answer::Progress(step) => {
                let title = match &self.view {
                    View::Waiting { title, .. } | View::Progress { title, .. } => title.clone(),
                    _ => String::new(),
                };
                let mut steps = match std::mem::replace(&mut self.view, View::Catalog) {
                    View::Progress { steps, .. } => steps,
                    _ => Vec::new(),
                };
                steps.push(step);
                self.view = View::Progress { title, steps };
            }
            Answer::Installed { app, title, home, restarts } => {
                let place = home_place(&home, &app).unwrap_or(Place { page: 0, pages: 0, row: 0 });
                self.view = View::Done { title, place, restarts };
                return self.refresh();
            }
            Answer::Removed { app } => {
                let title = self.cache.installed.iter().find(|i| i.app == app).map(|i| i.title.clone()).unwrap_or(app);
                self.view = View::Removed { title };
                return self.refresh();
            }
            Answer::Failed(why) => {
                let from = match &self.view {
                    View::Busy { from, .. } | View::Details { from, .. } | View::Approve { from, .. } => *from,
                    View::Installed => From::Installed,
                    _ => From::Catalog,
                };
                self.view = View::Failed { why, from };
            }
        }
        Vec::new()
    }

    /// Asks for the catalog again, as what is installed has changed.
    fn refresh(&mut self) -> Vec<Effect> {
        self.cache = Cache::default();
        self.ask_page(0)
    }

    fn pages(n: usize, per: usize) -> usize {
        n.div_ceil(per).max(1)
    }

    /// The listing with `krate`'s newest version, if it has come.
    fn listing(&self, krate: &str) -> Option<&Listing> {
        self.cache.apps.iter().flatten().find(|l| l.krate == krate)
    }

    /// What the tap on `a` does.
    pub fn act(&mut self, a: Action, rows: usize) -> Vec<Effect> {
        let back_to = |from: From| if from == From::Installed { View::Installed } else { View::Catalog };
        match a {
            Action::Exit => return vec![Effect::Exit],
            Action::Retry => {
                self.conn = Conn::Connecting;
                return vec![Effect::Connect];
            }
            Action::ShowInstalled => self.view = View::Installed,
            Action::ShowCatalog => self.view = View::Catalog,
            Action::Prev | Action::Next => {
                let forward = a == Action::Next;
                let (page, n) = match self.view {
                    View::Installed => (&mut self.installed_page, self.cache.installed.len()),
                    _ => (&mut self.page, self.cache.total.unwrap_or(0)),
                };
                let pages = Self::pages(n, Self::per_page(rows));
                *page = if forward { (*page + 1) % pages } else { (*page + pages - 1) % pages };
                return self.fill(rows);
            }
            Action::Pick(i) => {
                if let Some(Some(l)) = self.cache.apps.get(i) {
                    let ask = Ask::Details { krate: l.krate.clone(), version: l.version.clone() };
                    self.view = View::Busy { what: format!("Looking up {}", name(l)), from: From::Catalog };
                    return vec![Effect::Send(ask)];
                }
            }
            Action::PickInstalled(i) => {
                if let Some(inst) = self.cache.installed.get(i) {
                    let version = self.listing(&inst.krate).map_or(inst.version.clone(), |l| l.version.clone());
                    let ask = Ask::Details { krate: inst.krate.clone(), version };
                    self.view = View::Busy { what: format!("Looking up {}", inst.title), from: From::Installed };
                    return vec![Effect::Send(ask)];
                }
            }
            Action::Back | Action::Cancel => {
                let was_waiting = matches!(self.view, View::Waiting { .. });
                self.view = match std::mem::replace(&mut self.view, View::Catalog) {
                    View::Approve { d, from } => View::Details { d, from },
                    View::ApproveRemove { from, .. } => back_to(from),
                    View::Details { from, .. } | View::Failed { from, .. } | View::Busy { from, .. } => back_to(from),
                    _ => View::Catalog,
                };
                if was_waiting {
                    return vec![Effect::Send(Ask::Cancel)];
                }
            }
            Action::Get => {
                if let View::Details { d, from } = &self.view
                    && installable(d)
                {
                    self.view = View::Approve { d: d.clone(), from: *from };
                }
            }
            Action::Allow => {
                if let View::Approve { d, from } = &self.view {
                    let ask = Ask::Install {
                        krate: d.krate.clone(),
                        version: d.version.clone(),
                        access: d.access.clone(),
                        network: d.network.clone(),
                    };
                    self.view = View::Busy { what: format!("Installing {}", d.title), from: *from };
                    return vec![Effect::Send(ask)];
                }
            }
            Action::Remove => {
                if let View::Details { d, from } = &self.view
                    && d.installed.is_some()
                {
                    self.view = View::ApproveRemove { app: d.app.clone(), title: d.title.clone(), from: *from };
                }
            }
            Action::AllowRemove => {
                if let View::ApproveRemove { app, title, from } = &self.view {
                    let ask = Ask::Remove { app: app.clone() };
                    self.view = View::Busy { what: format!("Removing {title}"), from: *from };
                    return vec![Effect::Send(ask)];
                }
            }
        }
        Vec::new()
    }

    /// The screen, `cols` by `rows`.
    pub fn draw(&self, cols: usize, rows: usize) -> Frame<Action> {
        let mut f = Frame::new(cols, rows);
        match &self.view {
            View::Catalog => self.draw_catalog(&mut f),
            View::Installed => self.draw_installed(&mut f),
            View::Busy { what, .. } => {
                head(&mut f, "GET APPS", "");
                f.put(1, 2, &format!("{what} on the server {}", spinner(self.tick)), TEXT);
                bar(&mut f, &[("CANCEL", Action::Back, BUTTON)], self.pressed);
            }
            View::Details { d, .. } => self.draw_details(&mut f, d),
            View::Approve { d, .. } => self.draw_approve(&mut f, d),
            View::ApproveRemove { title, .. } => {
                head(&mut f, &format!("REMOVE {title}?"), "");
                let text = format!(
                    "{title} and what it keeps go from the server's tree, for every Chumby that uses it. Allowing it here asks your neochumbyd, where its owner approves it too."
                );
                for (i, l) in wrap(&text, cols - 2, 8).iter().enumerate() {
                    f.put(1, 2 + i, l, TEXT);
                }
                bar(&mut f, &[("CANCEL", Action::Cancel, BUTTON), ("REMOVE", Action::AllowRemove, STOP)], self.pressed);
            }
            View::Waiting { code, minutes, title, remove } => {
                head(&mut f, "ASK THE SERVER", spinner(self.tick));
                let done = if *remove { "removed" } else { "installed" };
                let mut y = 2;
                f.put(1, y, "On your neochumbyd's server, run:", TEXT);
                y += 2;
                f.put(3, y, &format!("neochumbyd approve {code}"), CODE);
                y += 2;
                for l in wrap(&format!("and {title} is {done}. To say no instead:"), cols - 2, 2) {
                    f.put(1, y, &l, TEXT);
                    y += 1;
                }
                y += 1;
                f.put(3, y, &format!("neochumbyd refuse {code}"), CODE);
                y += 2;
                f.put(1, y, &format!("This request lapses in {minutes} minutes."), DIM);
                bar(&mut f, &[("CANCEL", Action::Cancel, BUTTON)], self.pressed);
            }
            View::Progress { title, steps } => {
                head(&mut f, &format!("INSTALLING {title}"), spinner(self.tick));
                for (i, s) in steps.iter().enumerate() {
                    f.put(1, 2 + i, &format!("- {s}"), TEXT);
                }
            }
            View::Done { title, place, restarts } => {
                head(&mut f, "INSTALLED", "");
                let mut lines = vec![(format!("{title} is installed."), TITLE)];
                if place.page > 0 {
                    let page = if place.pages > 1 {
                        format!("page {} of {}, ", place.page, place.pages)
                    } else {
                        String::new()
                    };
                    lines.push((format!("Find it in home: {page}row {}.", place.row), TEXT));
                }
                let when = if *restarts {
                    "Leaving GET APPS starts the Chumby's apps again, so that home and the session have it."
                } else {
                    "It reaches this Chumby's stick with its next sync, and home lists it from the start after that."
                };
                lines.push((when.into(), DIM));
                let mut y = 2;
                for (text, style) in lines {
                    for l in wrap(&text, cols - 2, 4) {
                        f.put(1, y, &l, style);
                        y += 1;
                    }
                    y += 1;
                }
                bar(&mut f, &[("MORE APPS", Action::ShowCatalog, BUTTON), ("HOME", Action::Exit, GO)], self.pressed);
            }
            View::Removed { title } => {
                head(&mut f, "REMOVED", "");
                f.put(1, 2, &format!("{title} is removed."), TITLE);
                bar(&mut f, &[("MORE APPS", Action::ShowCatalog, BUTTON), ("HOME", Action::Exit, GO)], self.pressed);
            }
            View::Failed { why, .. } => {
                head(&mut f, "GET APPS", "");
                f.put(1, 2, "That didn't work:", BAD);
                for (i, l) in wrap(why, cols - 2, rows.saturating_sub(4 + BAR)).iter().enumerate() {
                    f.put(1, 4 + i, l, TEXT);
                }
                bar(&mut f, &[("BACK", Action::Back, BUTTON)], self.pressed);
            }
        }
        f
    }

    fn draw_catalog(&self, f: &mut Frame<Action>) {
        let (cols, rows) = (f.cols, f.rows);
        let message = |f: &mut Frame<Action>, title: &str, text: &str, style: Style| {
            f.put(1, 2, title, style);
            for (i, l) in wrap(text, cols - 2, rows.saturating_sub(4 + BAR)).iter().enumerate() {
                f.put(1, 4 + i, l, TEXT);
            }
        };
        match (&self.conn, self.cache.total) {
            (Conn::Connecting, _) | (Conn::Up, None) => {
                head(f, "GET APPS", "");
                f.put(1, 2, &format!("Asking neochumbyd {}", spinner(self.tick)), TEXT);
                bar(f, &[("EXIT", Action::Exit, BUTTON)], self.pressed);
            }
            (Conn::Down(why), _) => {
                head(f, "GET APPS", "");
                message(
                    f,
                    "Can't reach neochumbyd.",
                    &format!("GET APPS needs the neochumbyd this Chumby is paired with. {}", sentence(why)),
                    BAD,
                );
                bar(f, &[("TRY AGAIN", Action::Retry, BUTTON), ("EXIT", Action::Exit, BUTTON)], self.pressed);
            }
            (Conn::Up, Some(0)) => {
                head(f, "GET APPS", "");
                if self.cache.fetched_at == 0 {
                    message(
                        f,
                        "The catalog is empty.",
                        "neochumbyd hasn't fetched it yet. It does once a day; or, on the server: neochumbyd catalog refresh",
                        WARN,
                    );
                } else {
                    message(
                        f,
                        "No apps in the catalog yet.",
                        "Catalog apps are crates on crates.io with the keyword neochumby-app.",
                        WARN,
                    );
                }
                self.catalog_bar(f, 1);
            }
            (Conn::Up, Some(total)) => {
                let per = Self::per_page(rows);
                let first = self.page * per;
                let last = (first + per).min(total);
                head(f, "GET APPS", &format!("{}-{} of {total}", first + 1, last));
                for (row, i) in (first..last).enumerate() {
                    let y = 1 + row * ENTRY;
                    match self.cache.apps.get(i).cloned().flatten() {
                        Some(l) => {
                            self.draw_listing(f, y, &l, self.pressed == Some(Action::Pick(i)));
                            f.target(Rect { x: 0, y, w: cols, h: ENTRY - 1 }, Action::Pick(i));
                        }
                        None => {
                            f.put(1, y, &format!("... {}", spinner(self.tick)), DIM);
                        }
                    }
                }
                self.catalog_bar(f, Self::pages(total, per));
            }
        }
    }

    fn catalog_bar(&self, f: &mut Frame<Action>, pages: usize) {
        let installed = format!("INSTALLED {}", self.cache.installed.len());
        let mut buttons = vec![("EXIT", Action::Exit, BUTTON)];
        if pages > 1 {
            buttons.insert(0, ("<", Action::Prev, BUTTON));
        }
        buttons.push((&installed, Action::ShowInstalled, BUTTON));
        if pages > 1 {
            buttons.push((">", Action::Next, BUTTON));
        }
        bar(f, &buttons, self.pressed);
    }

    fn draw_listing(&self, f: &mut Frame<Action>, y: usize, l: &Listing, lit: bool) {
        let cols = f.cols;
        if lit {
            f.fill(Rect { x: 0, y, w: cols, h: ENTRY - 1 }, Style::PLAIN.on(Color::Blue));
        }
        let on = |s: Style| if lit { s.on(Color::Blue) } else { s };
        let (label, style) = match l.verified {
            Some(true) => ("verified build", VERIFIED),
            Some(false) => ("UNVERIFIED", UNVERIFIED),
            None if l.refused => ("can't install", BAD),
            None => ("not looked up", DIM),
        };
        let room = cols.saturating_sub(label.len() + 3);
        f.put(1, y, &cut(&name(l), room), on(TITLE));
        f.put_right(y, 1, label, on(style));
        let installed = self.cache.installed.iter().find(|i| i.krate == l.krate);
        let mut second = match &l.publisher[..] {
            "" => String::new(),
            p => format!("by {p}  "),
        };
        second += &format!("{} downloads  v{}", count(l.downloads), l.version);
        if let Some(i) = installed {
            second += if i.version == l.version { "  INSTALLED" } else { "  UPDATE" };
        }
        f.put(2, y + 1, &cut(&second, cols - 3), on(DIM));
        let desc = wrap(&l.description, cols - 3, 1).pop().unwrap_or_default();
        f.put(2, y + 2, &desc, on(TEXT));
    }

    fn draw_installed(&self, f: &mut Frame<Action>) {
        let (cols, rows) = (f.cols, f.rows);
        let all = &self.cache.installed;
        let per = Self::per_page(rows);
        let pages = Self::pages(all.len(), per);
        let first = self.installed_page.min(pages - 1) * per;
        let last = (first + per).min(all.len());
        head(
            f,
            "INSTALLED",
            &if all.is_empty() { String::new() } else { format!("{}-{} of {}", first + 1, last, all.len()) },
        );
        if all.is_empty() {
            f.put(1, 2, "No catalog apps are installed.", TEXT);
        }
        for (row, i) in (first..last).enumerate() {
            let y = 1 + row * ENTRY;
            let a = &all[i];
            let lit = self.pressed == Some(Action::PickInstalled(i));
            if lit {
                f.fill(Rect { x: 0, y, w: cols, h: ENTRY - 1 }, Style::PLAIN.on(Color::Blue));
            }
            let on = |s: Style| if lit { s.on(Color::Blue) } else { s };
            f.put(1, y, &a.title, on(TITLE));
            let newer = self.listing(&a.krate).filter(|l| l.version != a.version);
            if let Some(l) = newer {
                f.put_right(y, 1, &format!("v{} out", l.version), on(WARN));
            }
            f.put(2, y + 1, &cut(&format!("{} v{}", a.krate, a.version), cols - 3), on(DIM));
            f.put(2, y + 2, &cut(&format!("uses {}", plain_list(&a.access)), cols - 3), on(TEXT));
            f.target(Rect { x: 0, y, w: cols, h: ENTRY - 1 }, Action::PickInstalled(i));
        }
        let mut buttons = vec![("CATALOG", Action::ShowCatalog, BUTTON)];
        if pages > 1 {
            buttons.insert(0, ("<", Action::Prev, BUTTON));
            buttons.push((">", Action::Next, BUTTON));
        }
        bar(f, &buttons, self.pressed);
    }

    fn draw_details(&self, f: &mut Frame<Action>, d: &Details) {
        let cols = f.cols;
        let rows = f.rows;
        head(f, &d.title_or_name(), &format!("v{}", d.version));
        let mut y = 1;
        let mut line = |f: &mut Frame<Action>, x: usize, text: &str, style: Style| {
            if y < rows - BAR {
                f.put(x, y, text, style);
                y += 1;
            }
        };
        let by = if d.publisher.is_empty() { String::new() } else { format!("by {}", d.publisher) };
        line(f, 1, &cut(&by, cols.saturating_sub(17)), TEXT);
        let (label, style) = match (installable(d), d.verified) {
            (false, _) => ("can't install", BAD),
            (true, true) => ("verified build", VERIFIED),
            (true, false) => ("UNVERIFIED", UNVERIFIED),
        };
        f.put_right(1, 1, label, style);
        line(f, 1, &format!("{} downloads   {}", count(d.downloads), size(d.size)), DIM);
        for l in wrap(&d.description, cols - 2, 2) {
            line(f, 1, &l, TEXT);
        }
        line(f, 1, "", TEXT);
        line(f, 1, "It asks for:", TITLE);
        if d.access.is_empty() && d.refused.is_empty() && d.network.is_empty() {
            line(f, 3, "nothing beyond its own files", TEXT);
        }
        for w in &d.access {
            line(f, 3, &format!("- {}", word(w)), TEXT);
        }
        for w in &d.refused {
            line(f, 3, &cut(&format!("- {w}: REFUSED, never for a catalog app"), cols - 4), BAD);
        }
        if !d.network.is_empty() {
            line(f, 3, "- the internet, from the server, at:", TEXT);
            for h in &d.network {
                line(f, 7, &cut(h, cols - 8), TEXT);
            }
        }
        if !d.problem.is_empty() || !d.refused.is_empty() {
            let why = if d.problem.is_empty() { "it asks for what is refused".to_string() } else { d.problem.clone() };
            for l in wrap(&format!("Can't be installed: {why}"), cols - 2, 3) {
                line(f, 1, &l, BAD);
            }
        } else if !d.compiled {
            for l in wrap("SLOW: this server has no compiler for it (wamrc), so it would run interpreted.", cols - 2, 2)
            {
                line(f, 1, &l, WARN);
            }
        }
        match &d.installed {
            Some(v) if *v == d.version => line(f, 1, "Installed.", VERIFIED),
            Some(v) if d.more.is_empty() => line(f, 1, &format!("v{v} installed; this asks for nothing more."), TEXT),
            Some(v) => {
                line(f, 1, &cut(&format!("v{v} installed; this asks for more: {}", d.more.join(", ")), cols - 2), WARN)
            }
            None => {}
        }
        let get = if installable(d) { BUTTON } else { DIM_BUTTON };
        let mut buttons = vec![("BACK", Action::Back, BUTTON)];
        match &d.installed {
            None => buttons.push(("GET", Action::Get, if installable(d) { GO } else { get })),
            Some(v) => {
                buttons.push(("REMOVE", Action::Remove, STOP));
                if *v != d.version {
                    buttons.push(("UPDATE", Action::Get, if installable(d) { GO } else { get }));
                }
            }
        }
        bar(f, &buttons, self.pressed);
    }

    fn draw_approve(&self, f: &mut Frame<Action>, d: &Details) {
        let (cols, rows) = (f.cols, f.rows);
        let update = d.installed.is_some();
        let title = d.title_or_name();
        head(f, &format!("{} {title}?", if update { "UPDATE" } else { "INSTALL" }), &format!("v{}", d.version));
        let mut y = 2;
        let mut line = |f: &mut Frame<Action>, x: usize, text: &str, style: Style| {
            if y < rows - BAR {
                f.put(x, y, text, style);
                y += 1;
            }
        };
        if d.access.is_empty() {
            line(f, 1, &format!("{title} uses nothing beyond its own files."), TEXT);
        } else {
            line(f, 1, &format!("Allow {title} to use:"), TITLE);
            for w in &d.access {
                line(f, 3, &format!("- {}", word(w)), TEXT);
            }
        }
        if !d.network.is_empty() {
            line(f, 1, "Its companion, on the server, may reach:", TITLE);
            for h in &d.network {
                line(f, 3, &format!("- {h}"), TEXT);
            }
        }
        if update {
            let more = if d.more.is_empty() {
                "It asks for nothing new.".into()
            } else {
                format!("New: {}.", d.more.join(", "))
            };
            line(f, 1, &more, if d.more.is_empty() { TEXT } else { WARN });
        }
        if !d.verified {
            let why = if d.build.is_empty() { "nothing says which CI built it" } else { &d.build };
            for l in wrap(&format!("UNVERIFIED: {why}."), cols - 2, 3) {
                line(f, 1, &l, UNVERIFIED);
            }
        }
        if !d.compiled {
            line(f, 1, "It will run interpreted, slowly.", WARN);
        }
        let ask = if update && d.more.is_empty() {
            "Allowing it here updates it: it asks for nothing new."
        } else {
            "Allowing it here asks your neochumbyd; its owner approves it there too."
        };
        for l in wrap(ask, cols - 2, 2) {
            line(f, 1, &l, DIM);
        }
        bar(f, &[("CANCEL", Action::Cancel, BUTTON), ("ALLOW", Action::Allow, GO)], self.pressed);
    }
}

/// Whether an app can be installed, as far as its details say.
fn installable(d: &Details) -> bool {
    d.problem.is_empty() && d.refused.is_empty() && d.access.iter().all(|w| !NEVER.contains(&w.as_str()))
}

trait Named {
    fn title_or_name(&self) -> String;
}

impl Named for Details {
    fn title_or_name(&self) -> String {
        if self.title.is_empty() { self.app.to_uppercase() } else { self.title.clone() }
    }
}

/// A listing's title, or its name until the server has looked it up.
fn name(l: &Listing) -> String {
    if l.title.is_empty() { l.app.to_uppercase() } else { l.title.clone() }
}

/// The header: `title` on the left, `right` on the right.
fn head(f: &mut Frame<Action>, title: &str, right: &str) {
    let cols = f.cols;
    f.fill(Rect { x: 0, y: 0, w: cols, h: 1 }, HEAD);
    f.put(1, 0, &cut(title, cols.saturating_sub(right.len() + 3)), HEAD);
    f.put_right(0, 1, right, HEAD);
}

/// The buttons along the bottom, the screen's width shared between them.
fn bar(f: &mut Frame<Action>, buttons: &[(&str, Action, Style)], pressed: Option<Action>) {
    let (cols, rows) = (f.cols, f.rows);
    let n = buttons.len().max(1);
    let y = rows.saturating_sub(BAR);
    // Narrow buttons (< and >) take less room than the rest.
    let narrow = buttons.iter().filter(|(l, _, _)| l.len() == 1).count();
    let gaps = n - 1;
    let narrow_w = 7.min(cols / n);
    let wide = n - narrow;
    let wide_w = (cols - gaps - narrow * narrow_w).checked_div(wide).unwrap_or(0);
    let mut x = 0;
    for (i, (label, action, style)) in buttons.iter().enumerate() {
        let mut w = if label.len() == 1 { narrow_w } else { wide_w };
        if i == n - 1 {
            w = cols - x;
        }
        let style = if pressed == Some(*action) { LIT } else { *style };
        f.button(Rect { x, y, w, h: BAR }, label, style, *action);
        x += w + 1;
    }
}

/// A turning line, while something is awaited.
fn spinner(tick: usize) -> &'static str {
    ["|", "/", "-", "\\"][tick % 4]
}

/// `s` cut to `n` characters, with "..." if it was longer.
fn cut(s: &str, n: usize) -> String {
    neochumby_proto::catalog::cut(s, n)
}

/// What the server said, as a sentence.
fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || s.ends_with('.') { s.to_string() } else { format!("{s}.") }
}

/// A count as people say it: 1,234, or 1.2M from a million.
pub fn count(n: u64) -> String {
    if n >= 1_000_000 {
        return format!("{}.{}M", n / 1_000_000, n % 1_000_000 / 100_000);
    }
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A size in bytes as KB or MB.
pub fn size(bytes: u64) -> String {
    match bytes {
        0 => "size unknown".into(),
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{}.{} MB", b / (1024 * 1024), b % (1024 * 1024) * 10 / (1024 * 1024)),
    }
}

/// What an access word lets an app do, said plainly.
pub fn word(w: &str) -> String {
    match w {
        "fb" => "the screen (it draws it itself)".into(),
        "touch" => "the touchscreen".into(),
        "speaker" => "the speaker".into(),
        "accel" => "the accelerometer".into(),
        w => w.into(),
    }
}

/// Access words as a short list: "the screen and touch", or "nothing".
fn plain_list(words: &[String]) -> String {
    let short: Vec<&str> = words
        .iter()
        .map(|w| match w.as_str() {
            "fb" => "the screen",
            "touch" => "touch",
            "speaker" => "the speaker",
            "accel" => "the accelerometer",
            w => w,
        })
        .collect();
    match short.len() {
        0 => "nothing beyond its files".into(),
        1 => short[0].into(),
        n => format!("{} and {}", short[..n - 1].join(", "), short[n - 1]),
    }
}

#[cfg(test)]
mod tests;
