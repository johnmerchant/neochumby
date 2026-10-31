//! GET APPS's screens at Spleen 6x12 (53x20) and 5x8 (64x30), driven by
//! taps where they land and by the server's answers.

use neochumby_proto::catalog::{Answer, Ask, Details, Installed, Listing, Page};

use super::*;

const COLS: usize = 53;
const ROWS: usize = 20;

fn listing(i: usize) -> Listing {
    Listing {
        krate: format!("neochumby-app{i}"),
        version: "1.0.0".into(),
        app: format!("app{i}"),
        title: if i == 0 { "WEATHER".into() } else { String::new() },
        publisher: if i == 0 { "someone".into() } else { String::new() },
        description: "Forecasts for your city, by touch, with a long description that goes on".into(),
        downloads: 12_345,
        recent_downloads: 56,
        stars: Some(3),
        verified: if i == 0 {
            Some(true)
        } else if i == 1 {
            Some(false)
        } else {
            None
        },
        refused: i == 3,
    }
}

fn page(from: u32, total: u32) -> Answer {
    let apps = (from..(from + PAGE).min(total)).map(|i| listing(i as usize)).collect();
    Answer::Page(Page { fetched_at: 1, total, from, apps, installed: Vec::new(), compiles: true })
}

fn weather() -> Details {
    Details {
        krate: "neochumby-app0".into(),
        version: "1.0.0".into(),
        app: "app0".into(),
        title: "WEATHER".into(),
        publisher: "someone".into(),
        description: "Forecasts for your city".into(),
        downloads: 12_345,
        verified: true,
        access: vec!["fb".into(), "touch".into()],
        network: vec!["api.open-meteo.com".into()],
        companion: true,
        size: 180 * 1024,
        compiled: true,
        ..Default::default()
    }
}

/// Taps the screen where `label` is drawn: its first character's cell.
fn tap(app: &mut App, label: &str) -> Vec<Effect> {
    let f = app.draw(COLS, ROWS);
    // From the bottom up: the buttons before a title that says the same.
    let (x, y) = (0..ROWS)
        .rev()
        .find_map(|y| f.row(y).find(label).map(|x| (x, y)))
        .unwrap_or_else(|| panic!("no {label} on screen:\n{}", f.text()));
    let a = f.hit(x, y).unwrap_or_else(|| panic!("{label} at ({x}, {y}) isn't a target:\n{}", f.text()));
    app.act(a, ROWS)
}

fn connected(total: u32) -> App {
    let mut app = App::new();
    assert_eq!(app.connected(), [Effect::Send(Ask::List { from: 0 })]);
    assert_eq!(app.answer(page(0, total), ROWS), []);
    app
}

#[test]
fn the_catalog_a_page_at_a_time() {
    let mut app = connected(10);
    let f = app.draw(COLS, ROWS);
    let text = f.text();
    assert!(f.row(0).starts_with(" GET APPS") && f.row(0).ends_with("1-4 of 10"), "{text}");
    // Title and label; publisher, downloads and version; description.
    assert!(f.row(1).starts_with(" WEATHER") && f.row(1).ends_with("verified build"), "{text}");
    assert_eq!(f.row(2), "  by someone  12,345 downloads  v1.0.0");
    assert_eq!(f.row(3), "  Forecasts for your city, by touch, with a long...");
    assert!(f.row(5).starts_with(" APP1") && f.row(5).ends_with("UNVERIFIED"), "{text}");
    assert!(f.row(9).ends_with("not looked up"), "{text}");
    assert!(f.row(13).ends_with("can't install"), "{text}");
    assert_eq!(f.style(2, 1), Style::fg(Color::White).bold());
    // Big targets: each listing's three rows, and the bar's three.
    for y in 1..=3 {
        assert_eq!(f.hit(30, y), Some(Action::Pick(0)));
    }
    assert_eq!(f.hit(30, 4), None, "the row between listings is no one's");
    for y in 17..20 {
        assert_eq!(f.hit(0, y), Some(Action::Prev));
        assert_eq!(f.hit(52, y), Some(Action::Next));
    }

    tap(&mut app, ">");
    assert!(app.draw(COLS, ROWS).row(0).ends_with("5-8 of 10"));
    tap(&mut app, ">");
    assert!(app.draw(COLS, ROWS).row(0).ends_with("9-10 of 10"));
    tap(&mut app, ">");
    assert!(app.draw(COLS, ROWS).row(0).ends_with("1-4 of 10"), "round to the first");
    tap(&mut app, "<");
    assert!(app.draw(COLS, ROWS).row(0).ends_with("9-10 of 10"), "and back to the last");
}

#[test]
fn pages_are_asked_for_as_they_are_shown() {
    let mut app = App::new();
    app.connected();
    let mut first = page(0, 40);
    if let Answer::Page(p) = &mut first {
        p.apps.truncate(32);
    }
    app.answer(first, ROWS);
    // 4 to a page: page 8 (from 32) needs the second page of the listing.
    for _ in 0..7 {
        assert_eq!(app.act(Action::Next, ROWS), []);
    }
    assert_eq!(app.act(Action::Next, ROWS), [Effect::Send(Ask::List { from: 32 })]);
    assert!(app.draw(COLS, ROWS).row(1).starts_with(" ..."));
    // Asked once only.
    app.act(Action::Prev, ROWS);
    assert_eq!(app.act(Action::Next, ROWS), []);
    app.answer(page(32, 40), ROWS);
    assert!(app.draw(COLS, ROWS).row(1).starts_with(" APP32"));
}

#[test]
fn spleen_5x8_fits_more() {
    let mut app = connected(10);
    let f = app.draw(64, 30);
    assert!(f.row(0).ends_with("1-6 of 10"), "{}", f.text());
    assert_eq!(f.hit(0, 29), Some(Action::Prev));
    app.act(Action::Next, 30);
    assert!(app.draw(64, 30).row(0).ends_with("7-10 of 10"));
}

#[test]
fn empty_and_unreachable_say_so() {
    let mut app = App::new();
    app.connected();
    app.answer(Answer::Page(Page::default()), ROWS);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("The catalog is empty.") && t.contains("neochumbyd catalog refresh"), "{t}");

    let mut app = connected(0);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("No apps in the catalog yet."), "{t}");

    app.gone("not paired");
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("Can't reach neochumbyd.") && t.contains("not paired."), "{t}");
    assert_eq!(tap(&mut app, "TRY AGAIN"), [Effect::Connect]);
    assert_eq!(app.conn, Conn::Connecting);
    app.gone("connection refused");
    assert_eq!(tap(&mut app, "EXIT"), [Effect::Exit]);
}

#[test]
fn details_then_install_approved_on_the_chumby_then_the_server() {
    let mut app = connected(3);
    let effects = tap(&mut app, "WEATHER");
    assert_eq!(effects, [Effect::Send(Ask::Details { krate: "neochumby-app0".into(), version: "1.0.0".into() })]);
    assert!(app.draw(COLS, ROWS).text().contains("Looking up WEATHER on the server"));
    app.answer(Answer::Details(Box::new(weather())), ROWS);

    let f = app.draw(COLS, ROWS);
    let t = f.text();
    assert!(f.row(0).starts_with(" WEATHER") && f.row(0).ends_with("v1.0.0"), "{t}");
    assert!(f.row(1).starts_with(" by someone") && f.row(1).ends_with("verified build"), "{t}");
    assert!(f.row(2).contains("12,345 downloads   180 KB"), "{t}");
    assert!(t.contains("- the screen (it draws it itself)\n   - the touchscreen\n"), "{t}");
    assert!(t.contains("- the internet, from the server, at:\n       api.open-meteo.com\n"), "{t}");
    assert!(!t.contains("SLOW"), "{t}");

    tap(&mut app, "GET");
    let t = app.draw(COLS, ROWS).text();
    assert!(t.starts_with(" INSTALL WEATHER?"), "{t}");
    assert!(t.contains("Allow WEATHER to use:") && t.contains("- api.open-meteo.com"), "{t}");
    // Cancel goes back to the details, and nothing is sent.
    assert_eq!(tap(&mut app, "CANCEL"), []);
    assert!(matches!(app.view, View::Details { .. }));
    tap(&mut app, "GET");
    let effects = tap(&mut app, "ALLOW");
    assert_eq!(
        effects,
        [Effect::Send(Ask::Install {
            krate: "neochumby-app0".into(),
            version: "1.0.0".into(),
            access: vec!["fb".into(), "touch".into()],
            network: vec!["api.open-meteo.com".into()],
        })]
    );

    app.answer(Answer::Waiting { code: "ABCD-EF23".into(), minutes: 10 }, ROWS);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("neochumbyd approve ABCD-EF23") && t.contains("neochumbyd refuse ABCD-EF23"), "{t}");
    assert!(t.contains("and WEATHER is installed."), "{t}");

    app.answer(Answer::Progress("Approved on the server.".into()), ROWS);
    app.answer(Answer::Progress("Compiling it for the Chumby.".into()), ROWS);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.starts_with(" INSTALLING WEATHER"), "{t}");
    assert!(t.contains("- Approved on the server.\n - Compiling it for the Chumby.\n"), "{t}");

    let home: Vec<String> = ["airplay", "app0", "clock", "doom", "logs"].map(String::from).to_vec();
    let installed =
        |restarts| Answer::Installed { app: "app0".into(), title: "WEATHER".into(), home: home.clone(), restarts };
    let effects = app.answer(installed(true), ROWS);
    assert_eq!(effects, [Effect::Send(Ask::List { from: 0 })], "what's installed has changed");
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("WEATHER is installed.") && t.contains("Find it in home: page 1 of 2, row 2."), "{t}");
    assert!(t.contains("Leaving GET APPS starts the Chumby's apps again"), "{t}");
    app.answer(installed(false), ROWS);
    assert!(app.draw(COLS, ROWS).text().contains("with its next sync"));
    assert_eq!(tap(&mut app, "HOME"), [Effect::Exit]);
}

#[test]
fn cancelling_a_request_takes_it_back() {
    let mut app = connected(1);
    tap(&mut app, "WEATHER");
    app.answer(Answer::Details(Box::new(weather())), ROWS);
    tap(&mut app, "GET");
    tap(&mut app, "ALLOW");
    app.answer(Answer::Waiting { code: "ABCD-EF23".into(), minutes: 10 }, ROWS);
    assert_eq!(tap(&mut app, "CANCEL"), [Effect::Send(Ask::Cancel)]);
    assert_eq!(app.view, View::Catalog);

    // Cancelled while the server was still checking it: its request, when
    // it comes, is taken back.
    tap(&mut app, "WEATHER");
    app.answer(Answer::Details(Box::new(weather())), ROWS);
    tap(&mut app, "GET");
    tap(&mut app, "ALLOW");
    assert_eq!(tap(&mut app, "CANCEL"), []);
    let late = app.answer(Answer::Waiting { code: "ABCD-EF23".into(), minutes: 10 }, ROWS);
    assert_eq!(late, [Effect::Send(Ask::Cancel)]);
    assert_eq!(app.view, View::Catalog);
}

#[test]
fn refused_access_is_shown_and_cannot_be_got() {
    let mut app = connected(1);
    tap(&mut app, "WEATHER");
    let d = Details {
        access: vec!["touch".into()],
        refused: vec!["root".into(), "terminal".into()],
        network: Vec::new(),
        problem: "it asks for \"root\", which a catalog app is never granted".into(),
        ..weather()
    };
    app.answer(Answer::Details(Box::new(d)), ROWS);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("- root: REFUSED, never for a catalog app"), "{t}");
    assert!(t.contains("- terminal: REFUSED"), "{t}");
    assert!(t.contains("Can't be installed:"), "{t}");
    assert_eq!(tap(&mut app, "GET"), []);
    assert!(matches!(app.view, View::Details { .. }), "GET does nothing");
}

#[test]
fn slow_without_a_compiled_build() {
    let mut app = connected(1);
    tap(&mut app, "WEATHER");
    app.answer(Answer::Details(Box::new(Details { compiled: false, ..weather() })), ROWS);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("SLOW: this server has no compiler"), "{t}");
    tap(&mut app, "GET");
    assert!(app.draw(COLS, ROWS).text().contains("It will run interpreted, slowly."));
}

#[test]
fn installed_apps_update_and_remove() {
    let mut app = App::new();
    app.connected();
    let installed = Installed {
        app: "app0".into(),
        title: "WEATHER".into(),
        krate: "neochumby-app0".into(),
        version: "0.9.0".into(),
        access: vec!["fb".into(), "touch".into()],
    };
    app.answer(
        Answer::Page(Page {
            fetched_at: 1,
            total: 1,
            from: 0,
            apps: vec![listing(0)],
            installed: vec![installed],
            compiles: true,
        }),
        ROWS,
    );
    assert!(app.draw(COLS, ROWS).row(2).ends_with("UPDATE"));
    tap(&mut app, "INSTALLED 1");
    let f = app.draw(COLS, ROWS);
    assert!(f.row(1).starts_with(" WEATHER") && f.row(1).ends_with("v1.0.0 out"), "{}", f.text());
    assert_eq!(f.row(3), "  uses the screen and touch");
    // Its details are the newest version's.
    let effects = tap(&mut app, "WEATHER");
    assert_eq!(effects, [Effect::Send(Ask::Details { krate: "neochumby-app0".into(), version: "1.0.0".into() })]);
    let d = Details { installed: Some("0.9.0".into()), more: vec!["speaker".into()], ..weather() };
    app.answer(Answer::Details(Box::new(d)), ROWS);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("v0.9.0 installed; this asks for more: speaker"), "{t}");
    tap(&mut app, "UPDATE");
    let t = app.draw(COLS, ROWS).text();
    assert!(t.starts_with(" UPDATE WEATHER?") && t.contains("New: speaker."), "{t}");
    tap(&mut app, "CANCEL");
    tap(&mut app, "REMOVE");
    assert!(app.draw(COLS, ROWS).text().starts_with(" REMOVE WEATHER?"));
    assert_eq!(tap(&mut app, "REMOVE"), [Effect::Send(Ask::Remove { app: "app0".into() })]);
    app.answer(Answer::Waiting { code: "ABCD-EF23".into(), minutes: 10 }, ROWS);
    assert!(app.draw(COLS, ROWS).text().contains("and WEATHER is removed."));
    app.answer(Answer::Progress("Approved on the server.".into()), ROWS);
    app.answer(Answer::Removed { app: "app0".into() }, ROWS);
    assert!(app.draw(COLS, ROWS).text().contains("WEATHER is removed."));
    tap(&mut app, "MORE APPS");
    assert_eq!(app.view, View::Catalog);
}

#[test]
fn failures_go_back_where_they_came_from() {
    let mut app = connected(1);
    tap(&mut app, "INSTALLED 0");
    assert!(app.draw(COLS, ROWS).text().contains("No catalog apps are installed."));
    tap(&mut app, "CATALOG");
    tap(&mut app, "WEATHER");
    app.answer(Answer::Failed("crates.io's index: HTTP 503".into()), ROWS);
    let t = app.draw(COLS, ROWS).text();
    assert!(t.contains("That didn't work:") && t.contains("HTTP 503"), "{t}");
    tap(&mut app, "BACK");
    assert_eq!(app.view, View::Catalog);
}

#[test]
fn a_held_target_is_lit() {
    let mut app = connected(1);
    app.pressed = Some(Action::Pick(0));
    let f = app.draw(COLS, ROWS);
    assert_eq!(f.style(40, 2).bg, Color::Blue);
    app.pressed = Some(Action::ShowInstalled);
    let f = app.draw(COLS, ROWS);
    assert_eq!(f.style(30, 18), LIT);
}

#[test]
fn where_home_lists_it() {
    let listed = |n: usize| (0..n).map(|i| format!("a{i}")).collect::<Vec<_>>();
    assert_eq!(home_place(&listed(4), "a3"), Some(Place { page: 1, pages: 1, row: 4 }));
    assert_eq!(home_place(&listed(5), "a3"), Some(Place { page: 2, pages: 2, row: 1 }));
    assert_eq!(home_place(&listed(7), "a6"), Some(Place { page: 3, pages: 3, row: 1 }));
    assert_eq!(home_place(&listed(2), "b"), None);
}

#[test]
fn numbers_said_plainly() {
    assert_eq!(count(0), "0");
    assert_eq!(count(999), "999");
    assert_eq!(count(1234), "1,234");
    assert_eq!(count(123_456), "123,456");
    assert_eq!(count(1_234_567), "1.2M");
    assert_eq!(size(500), "500 bytes");
    assert_eq!(size(180 * 1024), "180 KB");
    assert_eq!(size(3 * 1024 * 1024 / 2), "1.5 MB");
}

#[test]
fn only_ascii_reaches_the_screen() {
    let mut app = connected(1);
    if let Some(Some(l)) = app.cache.apps.get_mut(0) {
        l.description = "Caf\u{e9} \u{2014} \u{1f600}".into();
    }
    let f = app.draw(COLS, ROWS);
    assert!(f.ansi().bytes().all(|b| b == 0x1b || (0x20..0x7f).contains(&b)));
    assert_eq!(f.row(3), "  Caf? - ?");
}
