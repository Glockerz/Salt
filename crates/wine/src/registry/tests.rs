use super::*;

const USER_REG: &str = r#"WINE REGISTRY Version 2
;; All keys relative to REGISTRY\\User\\S-1-5-21-0-0-0-1000

#arch=win64

[Software\\Roblox\\RobloxStudio] 1729000000
#time=1db1e2f3a4b5c6d
"ContentFolder"="C:\\Program Files\\Roblox\\content"
"Layout"="<xml a=\"b\">\nline</xml>"
"Count"=dword:0000002a
@="default"

[Software\\Wine\\Credential Manager] 1729000000
#time=1db1e2f3a4b5c6d
"EncryptionKey"=hex:01,02,03,04,05,06,07,08,09,0a,0b,0c,0d,0e,0f,10,11,12,13,14,\
  15,16
"Multi"=str(7):"a\0b\0"
"Expand"=str(2):"%SystemRoot%\\x"

[Software\\Doomed] 1729000000
"x"=dword:1

[-Software\\Doomed]
"#;

#[test]
fn parse_wine_file() {
	let k = parse(USER_REG).unwrap();
	assert_eq!(k.name, "HKEY_CURRENT_USER");
	let studio = k.query(r"Software\Roblox\RobloxStudio").unwrap();
	assert_eq!(studio.modified, 0x1db1e2f3a4b5c6d);
	assert_eq!(
		studio.value("ContentFolder").unwrap().data,
		Data::Sz(r"C:\Program Files\Roblox\content".into())
	);
	assert_eq!(
		studio.value("Layout").unwrap().data,
		Data::Sz("<xml a=\"b\">\nline</xml>".into())
	);
	assert_eq!(studio.value("Count").unwrap().data, Data::Dword(42));
	assert_eq!(studio.value("").unwrap().data, Data::Sz("default".into()));

	let cred = k.query(r"Software\Wine\Credential Manager").unwrap();
	assert_eq!(cred.value("EncryptionKey").unwrap().data.as_bytes().unwrap().len(), 22);
	assert_eq!(cred.value("Multi").unwrap().data, Data::MultiSz(vec!["a".into(), "b".into()]));
	assert_eq!(cred.value("Expand").unwrap().data, Data::ExpandSz(r"%SystemRoot%\x".into()));

	assert!(k.query(r"Software\Doomed").is_none());
	assert!(k.query(r"Software\Wine").is_some());
}

#[test]
fn registry_query_roots() {
	let reg = Registry {
		current_user: parse(USER_REG).unwrap(),
		machine: Key::named("HKEY_LOCAL_MACHINE"),
	};
	assert!(reg.query(r"HKCU\Software\Roblox\RobloxStudio").is_some());
	assert!(reg.query(r"HKEY_CURRENT_USER\Software\Roblox").is_some());
	assert!(reg.query(r"HKLM\Software").is_none());
	assert!(reg.query(r"HKCR\Foo").is_none());
}

#[test]
fn export_roundtrip() {
	let src = parse(USER_REG).unwrap();
	let mut stripped = Key::with_path(r"HKCU\");
	let path = r"Software\Roblox\RobloxStudio";
	stripped.add_key(path, src.query(path).unwrap());
	let path = r"Software\Wine\Credential Manager";
	stripped.add_key(path, src.query(path).unwrap());

	let text = String::from_utf8(stripped.export_bytes()).unwrap();
	assert!(text.starts_with("Windows Registry Editor Version 5.00\n"));
	assert!(text.contains("\n[HKEY_CURRENT_USER\\Software\\Roblox\\RobloxStudio]\n"));
	assert!(text.contains("\"Layout\"=\"<xml a=\\\"b\\\">\\nline</xml>\"\n"));
	assert!(text.contains("\"Count\"=dword:0000002a\n"));
	// 22 bytes wrap after the 76th column like regedit.
	assert!(text.contains("\"EncryptionKey\"=hex:01,02,03,04,05,06,07,08,09,0a,0b,0c,0d,0e,0f,10,11,12,13,\\\n  14,15,16\n"));

	// The export parses back into the same values.
	let back = parse(&text).unwrap();
	let a = back.query(r"HKEY_CURRENT_USER\Software\Roblox\RobloxStudio").unwrap();
	let b = src.query(r"Software\Roblox\RobloxStudio").unwrap();
	assert_eq!(a.values, b.values);
	let a = back.query(r"HKEY_CURRENT_USER\Software\Wine\Credential Manager").unwrap();
	let b = src.query(r"Software\Wine\Credential Manager").unwrap();
	assert_eq!(a.values, b.values);
}

#[test]
fn export_deletions() {
	let mut k = Key::with_path(r"HKCU\Software\Microsoft\ThemeManager");
	let tm = k.add(r"Software\Microsoft\ThemeManager");
	tm.set("ColorName", Data::Delete).set("ThemeActive", "0");
	k.add(r"Software\Empty");
	let text = String::from_utf8(k.export_bytes()).unwrap();
	assert!(text.contains("\"ColorName\"=-\n"));
	assert!(text.contains("\"ThemeActive\"=\"0\"\n"));
	assert!(text.contains("[-HKEY_CURRENT_USER\\Software\\Empty]"));
}

#[test]
fn long_lines() {
	let long = "x".repeat(200_000);
	let src = format!("{HEADER_WINE}\n\n[A] 1\n\"v\"=\"{long}\"\n");
	let k = parse(&src).unwrap();
	assert_eq!(k.query("A").unwrap().value("v").unwrap().data, Data::Sz(long));
}

#[test]
fn escapes() {
	assert_eq!(unescape(r#"a\\b\"c\n\x41\101"#), "a\\b\"c\nAA");
	assert_eq!(unescape(r"\xd83d\xde00"), "😀");
	assert_eq!(escape("a\\b\"c\n", true, false), r#"a\\b\"c\n"#);
	assert_eq!(escape("é", true, false), r"\xe9");
	assert_eq!(escape("é1", true, false), r"\x00e91");
	assert_eq!(escape("é", true, true), "é");
	assert_eq!(escape("[k]", false, false), r"\[k\]");
	assert_eq!(unescape(&escape("tab\there 😀 \\ \"q\"", true, false)), "tab\there 😀 \\ \"q\"");
}

#[test]
fn delete_and_add() {
	let mut k = Key::with_path(r"HKCU\A\B\C");
	assert!(k.query(r"A\B\C").is_some());
	assert!(k.delete(r"A\B"));
	assert!(k.query(r"A\B").is_none());
	assert!(!k.delete(r"A\B"));
	assert_eq!(k.query("A").unwrap().subkeys.len(), 0);
}

/// Parses a real prefix: `VINEGAR_TEST_PREFIX=/path/to/prefix cargo test -- --ignored`
#[test]
#[ignore]
fn parse_real_prefix() {
	let dir = std::env::var("VINEGAR_TEST_PREFIX").expect("VINEGAR_TEST_PREFIX");
	let t = std::time::Instant::now();
	let reg = Registry::load(std::path::Path::new(&dir)).unwrap();
	eprintln!("parsed in {:?}", t.elapsed());
	assert_eq!(reg.current_user.name, "HKEY_CURRENT_USER");
	assert_eq!(reg.machine.name, "HKEY_LOCAL_MACHINE");
	assert!(reg.query(r"HKLM\Software\Microsoft").is_some());
}
