//! WAMR as the runner uses it, on the host's interpreter: host functions,
//! imports, memory bounds, the memory cap and exits. One test, as WAMR is
//! one per process.

use neochumby_wamr::{Caller, Error, Func, ImportKind, Runtime, Trap, Val};

/// What the host functions see: the test's own record.
#[derive(Default)]
struct Seen {
    logged: Vec<String>,
}

fn log(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let s = String::from_utf8_lossy(c.bytes(a[0] as u32, a[1] as u32)?).into_owned();
    c.state::<Seen>().ok_or_else(|| Trap::Other("no state".into()))?.logged.push(s);
    Ok(0)
}

fn fill(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    c.bytes_mut(a[0] as u32, a[1] as u32)?.fill(0xab);
    Ok(0)
}

fn exit(_: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    Err(Trap::Exit(a[0] as u32))
}

fn add(_: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    Ok((a[0] as i64 + a[1] as i64) as u64)
}

static HOST: [Func; 4] = [
    Func { name: "log", params: &[Val::I32, Val::I32], result: None, call: log },
    Func { name: "fill", params: &[Val::I32, Val::I32], result: None, call: fill },
    Func { name: "exit", params: &[Val::I32], result: None, call: exit },
    Func { name: "add", params: &[Val::I64, Val::I64], result: Some(Val::I64), call: add },
];

fn wasm(text: &str) -> Vec<u8> {
    wat::parse_str(text).unwrap()
}

#[test]
fn runs_modules_inside_their_bounds() {
    assert!(neochumby_wamr::VERSION.starts_with(&format!("{}-", neochumby_wamr::release())));
    let rt = Runtime::init().unwrap();
    assert!(Runtime::init().is_err(), "one runtime a process");
    rt.register("host", &HOST).unwrap();
    assert_eq!(HOST[3].signature(), "(II)I");

    // A host function reads the module's memory, and gets its state.
    let m = rt
        .load(wasm(
            r#"(module
                (import "host" "log" (func $log (param i32 i32)))
                (memory 1)
                (data (i32.const 16) "hello")
                (func (export "_start") (call $log (i32.const 16) (i32.const 5))))"#,
        ))
        .unwrap();
    assert!(!m.is_aot());
    let imports = m.imports();
    assert_eq!(imports.len(), 1);
    assert_eq!((imports[0].module.as_str(), imports[0].name.as_str()), ("host", "log"));
    assert_eq!(imports[0].kind, ImportKind::Func);
    assert!(imports[0].linked);
    let mut i = m.instantiate(64 << 10, 16, Box::new(Seen::default())).unwrap();
    i.call("_start").unwrap();
    assert_eq!(i.state::<Seen>().unwrap().logged, ["hello"]);
    assert_eq!(i.call("nope"), Err(Error::NoExport("nope".into())));
    drop(i);
    drop(m);

    // An import nobody provides, or one of another type, is left
    // unlinked, for the runner to refuse.
    let m = rt
        .load(wasm(
            r#"(module
                (import "host" "socket" (func (param i32) (result i32)))
                (import "host" "log" (func (param i64)))
                (import "host" "add" (func (param i64 i64) (result i64))))"#,
        ))
        .unwrap();
    let linked: Vec<(String, bool)> = m.imports().into_iter().map(|i| (i.name, i.linked)).collect();
    assert_eq!(linked, [("socket".into(), false), ("log".into(), false), ("add".into(), true)]);
    drop(m);

    // Out of bounds, in the module and through the host: a trap, never a
    // read or write outside.
    let m = rt
        .load(wasm(
            r#"(module
                (import "host" "log" (func $log (param i32 i32)))
                (import "host" "fill" (func $fill (param i32 i32)))
                (memory 1)
                (func (export "load") (drop (i32.load (i32.const 65534))))
                (func (export "store") (i64.store (i32.const 65530) (i64.const 1)))
                (func (export "host_read") (call $log (i32.const 65530) (i32.const 7)))
                (func (export "host_write") (call $fill (i32.const 0xfffffff0) (i32.const 32)))
                (func (export "host_ok") (call $fill (i32.const 65520) (i32.const 16))))"#,
        ))
        .unwrap();
    let mut i = m.instantiate(64 << 10, 16, Box::new(Seen::default())).unwrap();
    for f in ["load", "store", "host_read", "host_write"] {
        match i.call(f) {
            Err(Error::Trap(why)) => assert!(why.contains("out of bounds"), "{f}: {why}"),
            r => panic!("{f}: {r:?}"),
        }
    }
    i.call("host_ok").unwrap();
    assert!(i.state::<Seen>().unwrap().logged.is_empty());
    drop(i);
    drop(m);

    // The memory cap: memory.grow past it fails inside the module.
    let m = rt
        .load(wasm(
            r#"(module
                (memory 1)
                (func (export "_start")
                  (if (i32.ne (memory.grow (i32.const 3)) (i32.const 1)) (then unreachable))
                  (if (i32.ne (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))))"#,
        ))
        .unwrap();
    let mut i = m.instantiate(64 << 10, 4, Box::new(())).unwrap();
    i.call("_start").unwrap();
    drop(i);
    drop(m);

    // An exit, and a trap of the module's own.
    let m = rt
        .load(wasm(
            r#"(module
                (import "host" "exit" (func $exit (param i32)))
                (import "host" "add" (func $add (param i64 i64) (result i64)))
                (func (export "_start")
                  (if (i64.ne (call $add (i64.const 40) (i64.const 2)) (i64.const 42)) (then unreachable))
                  (call $exit (i32.const 3)) unreachable)
                (func (export "boom") unreachable))"#,
        ))
        .unwrap();
    let mut i = m.instantiate(64 << 10, 16, Box::new(())).unwrap();
    assert_eq!(i.call("_start"), Err(Error::Exit(3)));
    match i.call("boom") {
        Err(Error::Trap(why)) => assert!(why.contains("unreachable"), "{why}"),
        r => panic!("{r:?}"),
    }
    drop(i);
    drop(m);

    // A start function runs inside instantiation, before the instance has
    // its state: a host call from it traps, and the instance isn't made.
    let m = rt
        .load(wasm(
            r#"(module
                (import "host" "log" (func $log (param i32 i32)))
                (memory 1)
                (func $s (call $log (i32.const 0) (i32.const 1)))
                (start $s))"#,
        ))
        .unwrap();
    match m.instantiate(64 << 10, 16, Box::new(Seen::default())) {
        Err(Error::Instantiate(why)) => assert!(why.contains("isn't ready"), "{why}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("instantiated"),
    }
    drop(m);

    // Not wasm at all.
    assert!(matches!(rt.load(b"\0asm garbage".to_vec()), Err(Error::Load(_))));
    assert!(matches!(rt.load(b"\x7fELF".to_vec()), Err(Error::Load(_))));
}
