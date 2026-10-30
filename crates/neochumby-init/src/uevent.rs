//! The kernel's device events, as it sends them on its netlink socket: a
//! summary line ("add@/devices/..."), then NUL-separated KEY=value pairs.

/// The part of an event ncinit acts on.
#[derive(Debug, PartialEq, Eq)]
pub struct Event<'a> {
    pub action: &'a str,
    pub modalias: Option<&'a str>,
}

/// Reads one message; None if it isn't a kernel event (udev's, which start
/// "libudev", or something malformed).
pub fn parse(msg: &[u8]) -> Option<Event<'_>> {
    let mut fields = msg.split(|&b| b == 0).filter(|f| !f.is_empty());
    let summary = std::str::from_utf8(fields.next()?).ok()?;
    if !summary.contains('@') {
        return None;
    }
    let mut action = None;
    let mut modalias = None;
    for field in fields {
        let Ok(field) = std::str::from_utf8(field) else { continue };
        match field.split_once('=') {
            Some(("ACTION", v)) => action = Some(v),
            Some(("MODALIAS", v)) if !v.is_empty() => modalias = Some(v),
            _ => {}
        }
    }
    Some(Event { action: action?, modalias })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_keyboard_arriving() {
        let msg =
            b"add@/devices/platform/soc/usb1/1-1/1-1:1.0\0ACTION=add\0DEVPATH=/devices/platform/soc/usb1/1-1/1-1:1.0\0\
SUBSYSTEM=usb\0DEVTYPE=usb_interface\0MODALIAS=usb:v0627p0001d0000dc00dsc00dp00ic03isc01ip01in00\0SEQNUM=812\0";
        assert_eq!(
            parse(msg),
            Some(Event { action: "add", modalias: Some("usb:v0627p0001d0000dc00dsc00dp00ic03isc01ip01in00") })
        );
    }

    #[test]
    fn reads_one_without_a_modalias() {
        assert_eq!(
            parse(b"remove@/devices/x\0ACTION=remove\0SUBSYSTEM=usb\0"),
            Some(Event { action: "remove", modalias: None })
        );
    }

    #[test]
    fn ignores_the_rest() {
        assert_eq!(parse(b"libudev\0\xfe\xed\xca\xfe"), None);
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"add@/x\0SUBSYSTEM=usb\0"), None);
        assert_eq!(parse(b"add@/x\0ACTION=add\0MODALIAS=\0"), Some(Event { action: "add", modalias: None }));
    }
}
