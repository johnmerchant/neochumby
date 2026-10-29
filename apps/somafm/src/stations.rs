//! The stations, compiled in from stations.txt: the same table the screen
//! is drawn from (render/render.py), so the list on the screen and the
//! streams played are always in the same order.

/// One station: SomaFM's id for it, its name and its stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Station {
    pub id: &'static str,
    pub title: &'static str,
    /// Plain-HTTP MP3: btplay, which plays it, has no TLS.
    pub url: &'static str,
}

const TABLE: &str = include_str!("../stations.txt");

/// The stations, in the order they're listed.
pub fn all() -> Vec<Station> {
    TABLE
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('|').map(str::trim);
            Some(Station { id: f.next()?, title: f.next()?, url: f.next()? })
        })
        .collect()
}

/// The stream of station `i`, if there is one.
pub fn url(i: usize) -> Option<&'static str> {
    all().get(i).map(|s| s.url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_plain_http_streams() {
        let s = all();
        assert_eq!(s.len(), 6);
        assert_eq!(
            s[0],
            Station { id: "groovesalad", title: "Groove Salad", url: "http://ice6.somafm.com/groovesalad-128-mp3" }
        );
        for st in &s {
            assert!(st.url.starts_with("http://"), "{}: btplay has no TLS", st.id);
            assert!(st.url.contains(st.id), "{}", st.id);
            // A line on the session's pipe: no spaces or line ends.
            assert!(!st.url.contains([' ', '\n', '\r']), "{}", st.id);
        }
        assert_eq!(url(5), Some("http://ice6.somafm.com/spacestation-128-mp3"));
        assert_eq!(url(6), None);
    }
}
