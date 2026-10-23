use super::compat::*;
use super::*;

const ID: &str = "ghmbeldphafepmbegfdlkpapadhbakde";

#[test]
fn bloqueurs_reconnus() {
    let ext = |id: &str, name: &str| Extension {
        id: id.into(),
        name: name.into(),
        description: String::new(),
        version: "1".into(),
        source: Source::Chrome,
        enabled: true,
        dir: id.into(),
        popup: None,
        options: None,
        icon: None,
        all_sites: true,
        firefox: false,
        engine_id: None,
        origin: None,
        permissions: Vec::new(),
        hosts: Vec::new(),
        optional: Vec::new(),
        granted: Vec::new(),
    };
    assert!(blocks_ads(&ext("ddkjiahejlhfcafbddmgiahcphecmpfh", "uBO Lite")));
    assert!(blocks_ads(&ext("autre", "AdGuard AdBlocker")));
    assert!(!blocks_ads(&ext(ID, "Proton Pass")));
}

#[test]
fn fiche_d_autorisations() {
    assert_eq!(permission_label("tabs").as_deref(), Some("Voir vos onglets et leurs adresses"));
    assert_eq!(permission_label("storage"), None);
    assert_eq!(permission_label("truc").as_deref(), Some("truc"));
    assert_eq!(permission_label("https://*/*").as_deref(), Some("Lire et modifier d'autres sites"));
    assert_eq!(labels(&["webRequest".into(), "webRequestBlocking".into(), "alarms".into()]), ["Observer les requêtes des pages"]);
    let hosts = ["https://*.proton.me/*".to_string(), "*://*.proton.me/x".into(), "http://a.fr/".into()];
    assert_eq!(site_labels(&hosts), ["*.proton.me", "a.fr"]);
}

#[test]
fn demandes_en_clair() {
    let ask = |items: &[&str]| ask_labels(&items.iter().map(|i| i.to_string()).collect::<Vec<_>>());
    // Sans conséquence pour l'utilisateur : accordé sans question.
    assert!(ask(&["storage", "alarms"]).is_empty());
    assert_eq!(ask(&["tabs"]), ["Voir vos onglets et leurs adresses"]);
    assert_eq!(ask(&["https://*.proton.me/*", "https://a.fr/*"]), ["Lire et modifier les pages de *.proton.me, a.fr"]);
    assert_eq!(
        ask(&["<all_urls>", "cookies", "https://a.fr/*"]),
        ["Lire et modifier les cookies des sites", "Lire et modifier toutes les pages que vous visitez"]
    );
    assert!(is_site_pattern("<all_urls>") && is_site_pattern("*://*/*") && !is_site_pattern("tabs"));
}

#[test]
fn nouvelles_versions() {
    assert!(is_newer("1.10.2", "1.9"));
    assert!(is_newer("2", "1.99.99"));
    assert!(!is_newer("1.2", "1.2.0"));
    assert!(!is_newer("1.2.3", "1.3"));
    let xml = r#"<?xml version="1.0"?><gupdate><app appid="x" status="ok"><updatecheck codebase="https://x/y.crx" version="2025.1.2" status="ok"/></app></gupdate>"#;
    assert_eq!(chrome_latest(xml).as_deref(), Some("2025.1.2"));
    assert_eq!(chrome_latest(r#"<app><updatecheck status="noupdate"/></app>"#), None);
    assert_eq!(chrome_latest("rien"), None);
}

#[test]
fn mise_a_jour_qui_demande_plus() {
    let base = Extension {
        id: ID.into(),
        name: "x".into(),
        description: String::new(),
        version: "1".into(),
        source: Source::Chrome,
        enabled: true,
        dir: ID.into(),
        popup: None,
        options: None,
        icon: None,
        all_sites: false,
        firefox: false,
        engine_id: None,
        origin: None,
        permissions: vec!["tabs".into()],
        hosts: vec!["https://a.fr/*".into()],
        optional: Vec::new(),
        granted: Vec::new(),
    };
    let mut same = base.clone();
    same.version = "2".into();
    assert!(!same.asks_more_than(&base));
    let mut more = same.clone();
    more.permissions.push("cookies".into());
    assert!(more.asks_more_than(&base));
    let mut all = same.clone();
    all.all_sites = true;
    assert!(all.asks_more_than(&base));
    // Ancienne installation sans `origin` : l'identifiant du Chrome Web Store sert.
    assert_eq!(base.update_origin(), Some(Origin::Chrome(ID.into())));
}

#[test]
fn dossiers_ranges_a_la_corbeille() {
    let root = std::env::temp_dir().join(format!("feather-corbeille-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let now = 1_000_000_000;
    for dir in ["utilisee-1/sous-dossier", "ancienne-2", ".nouvelle-3", ".corbeille/10-tres-ancienne-4"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    let recent = format!(".corbeille/{}-recente-5", now - 3600);
    fs::create_dir_all(root.join(&recent)).unwrap();
    fs::write(root.join(".nouvelle-3.telechargement"), b"x").unwrap();
    let used = Extension {
        id: ID.into(),
        name: "x".into(),
        description: String::new(),
        version: "1".into(),
        source: Source::Chrome,
        enabled: true,
        dir: "utilisee-1/sous-dossier".into(),
        popup: None,
        options: None,
        icon: None,
        all_sites: false,
        firefox: false,
        engine_id: None,
        origin: None,
        permissions: Vec::new(),
        hosts: Vec::new(),
        optional: Vec::new(),
        granted: Vec::new(),
    };
    sweep_in(&root, &[used], now);
    // L'extension de la liste reste ; l'ancienne version attend à la corbeille.
    assert!(root.join("utilisee-1/sous-dossier").is_dir());
    assert!(!root.join("ancienne-2").exists());
    assert!(root.join(format!(".corbeille/{now}-ancienne-2")).is_dir());
    // Installation interrompue : effacée tout de suite.
    assert!(!root.join(".nouvelle-3").exists() && !root.join(".nouvelle-3.telechargement").exists());
    // Une semaine à la corbeille, puis effacé.
    assert!(!root.join(".corbeille/10-tres-ancienne-4").exists());
    assert!(root.join(&recent).is_dir());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn bouton_du_store() {
    assert_eq!(store_page(&format!("https://chromewebstore.google.com/detail/proton-pass/{ID}?hl=fr")), Some(ID.into()));
    assert_eq!(store_page("https://chromewebstore.google.com/"), None);
    assert_eq!(store_page(&format!("https://exemple.fr/detail/x/{ID}")), None);
    assert_eq!(store_page(&format!("http://chromewebstore.google.com/detail/x/{ID}")), None);
    assert_eq!(store_page(ID), None);
}

#[test]
fn liens_compris() {
    let chrome = Origin::Chrome(ID.into());
    assert_eq!(parse_input(&format!("https://chromewebstore.google.com/detail/proton-pass/{ID}")), Ok(chrome));
    assert!(parse_input(&format!("https://chromewebstore.google.com/detail/proton-pass/{ID}?hl=fr")).is_ok());
    assert_eq!(parse_input(&format!("chrome.google.com/webstore/detail/x/{ID}")), Ok(Origin::Chrome(ID.into())));
    assert_eq!(parse_input(ID), Ok(Origin::Chrome(ID.into())));
    assert_eq!(
        parse_input("https://addons.mozilla.org/fr/firefox/addon/ublock-origin/"),
        Ok(Origin::Firefox("ublock-origin".into()))
    );
    assert!(parse_input("https://chromewebstore.google.com/").is_err());
    assert!(parse_input("https://exemple.fr/extension").is_err());
    assert!(parse_input("bonjour").is_err());
    assert!(firefox_url("a b").ends_with("/a+b/latest.xpi"));
}

#[test]
fn base64_aller_retour() {
    for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
        assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data);
    }
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert!(base64_decode("@@").is_none());
}

/// Un champ protobuf « longueur + octets ».
fn field(num: u64, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |mut v: u64| loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    };
    put((num << 3) | 2);
    put(value.len() as u64);
    out.extend_from_slice(value);
    out
}

/// Extension signée faite pour ces tests : zip de `essai/manifest.json`,
/// `essai/popup.html` et `essai/icons/32.png`, deux preuves RSA de 2048 bits
/// (celle de l'auteur, dont la clé donne l'identifiant, et une seconde comme
/// celle du Chrome Web Store), signées par openssl (dgst -sha256 -sign).
const SIGNED_CRX: &str = concat!(
    "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",
    "AAA=",
);
const SIGNED_ID: &str = "dldbbmnkgmaeidmakkhlcohgijjklcad";

/// Un `.crx` (version 3) fait à la main : preuves (clé, signature) et identifiant signé.
fn crx3(proofs: &[(&[u8], &[u8])], crx_id: Option<&[u8]>, zip: &[u8]) -> Vec<u8> {
    let mut header: Vec<u8> = proofs.iter().flat_map(|(key, sig)| field(2, &[field(1, key), field(2, sig)].concat())).collect();
    if let Some(id) = crx_id {
        header.extend(field(10000, &field(1, id)));
    }
    let mut crx = b"Cr24".to_vec();
    crx.extend(3u32.to_le_bytes());
    crx.extend((header.len() as u32).to_le_bytes());
    crx.extend(&header);
    crx.extend(zip);
    crx
}

#[test]
fn crx3_lu() {
    let crx = base64_decode(SIGNED_CRX).unwrap();
    let (zip, key) = crx_zip(&crx).unwrap();
    assert!(zip.starts_with(b"PK\x03\x04"));
    // La clé de l'auteur (celle de l'identifiant signé), pas la seconde.
    let key = key.unwrap();
    assert_eq!(id_from_key(&key), SIGNED_ID);
    assert!(is_store_id(&id_from_key(&key)));
    // Archive modifiée, ou une des signatures abîmée : refusé.
    let mut changed = crx.clone();
    let last = changed.len() - 30;
    changed[last] ^= 1;
    assert!(crx_zip(&changed).unwrap_err().contains("signature invalide"));
    let mut bad = crx.clone();
    bad[100] ^= 1;
    assert!(crx_zip(&bad).is_err());
    // La clé d'une autre extension (publique) avec une fausse signature : refusé.
    let id: Vec<u8> = Sha256::digest(&key)[..16].to_vec();
    assert!(crx_zip(&crx3(&[(&key[..], &b"fausse"[..])], Some(&id), zip)).is_err());
    // Sans identifiant signé : rien à vérifier, donc pas de clé.
    assert_eq!(crx_zip(&crx3(&[(&key[..], &b"fausse"[..])], None, zip)).unwrap().1, None);
    // Ancien format (version 2) : pas de clé non plus.
    let mut crx2 = b"Cr24".to_vec();
    for v in [2u32, 3, 2] {
        crx2.extend(v.to_le_bytes());
    }
    crx2.extend(b"clesg");
    crx2.extend(zip);
    assert_eq!(crx_zip(&crx2).unwrap(), (zip, None));
    // Zip seul (.xpi) : pas de clé ; autre chose : refusé.
    assert_eq!(crx_zip(b"PK\x03\x04abc").unwrap().1, None);
    assert!(crx_zip(b"<html>").is_err());
    assert!(crx_zip(b"Cr24\x03\x00\x00\x00\xff\xff\x00\x00").is_err());
}

#[test]
fn identifiant_chrome() {
    // Exemple connu : clé vide → empreinte e3b0c442… → « odlameecjipmbmbejkplpemijjgpljce ».
    assert_eq!(id_from_key(b""), "odlameecjipmbmbejkplpemijjgpljce");
}

#[test]
fn commentaires_retires() {
    let text = "{ // nom\n \"a\": \"http://x/*y*/\", /* bloc */ \"b\": 1 }";
    let v: Value = serde_json::from_str(&strip_comments(text)).unwrap();
    assert_eq!(v["a"], "http://x/*y*/");
    assert_eq!(v["b"], 1);
}

#[test]
fn manifeste_lu() {
    let dir = std::env::temp_dir().join(format!("feather-ext-{}", stamp()));
    fs::create_dir_all(dir.join("_locales/fr")).unwrap();
    fs::create_dir_all(dir.join("icons")).unwrap();
    fs::write(dir.join("popup.html"), "<p>").unwrap();
    fs::write(dir.join("icons/48.png"), "png").unwrap();
    fs::write(dir.join("icons/16.png"), "png").unwrap();
    fs::write(dir.join("_locales/fr/messages.json"), r#"{"appName":{"message":"Mon extension"}}"#).unwrap();
    fs::write(
        dir.join("manifest.json"),
        "\u{feff}{\"manifest_version\":3,\"name\":\"__MSG_appname__\",\"version\":\"1.2\",\"default_locale\":\"en\",
          \"action\":{\"default_popup\":\"popup.html\"},\"options_page\":\"../dehors.html\",
          \"icons\":{\"16\":\"icons/16.png\",\"48\":\"icons/48.png\"},
          \"host_permissions\":[\"<all_urls>\"],\"browser_specific_settings\":{\"gecko\":{\"id\":\"a@b\"}},
          \"permissions\":[\"tabs\",\"storage\",\"https://*.proton.me/*\"],\"optional_permissions\":[\"history\"],
          \"content_scripts\":[{\"matches\":[\"https://*.proton.me/*\",\"https://mail.exemple.fr/*\"]}]}",
    )
    .unwrap();
    let info = read_manifest(&dir).unwrap();
    assert_eq!(info.name, "Mon extension");
    assert_eq!(info.version, "1.2");
    assert_eq!(info.popup.as_deref(), Some("popup.html"));
    assert_eq!(info.options, None);
    assert_eq!(info.icon.as_deref(), Some("icons/48.png"));
    assert!(info.all_sites);
    assert_eq!(info.gecko_id.as_deref(), Some("a@b"));
    assert!(!info.has_key);
    // Les sites sont séparés des autorisations, sans doublons.
    assert_eq!(info.permissions, ["tabs", "storage"]);
    assert_eq!(info.hosts, ["https://*.proton.me/*", "<all_urls>", "https://mail.exemple.fr/*"]);
    assert_eq!(info.optional, ["history"]);
    add_key(&dir, b"cle").unwrap();
    assert!(read_manifest(&dir).unwrap().has_key);
    assert_eq!(manifest_root(&dir).unwrap(), dir);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn complements_avant_l_arriere_plan() {
    let dir = std::env::temp_dir().join(format!("feather-compat-{}", stamp()));
    let write = |manifest: &str| {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.json"), manifest).unwrap();
    };
    let manifest = || read_json(&dir.join("manifest.json")).unwrap();
    let wrapper = || fs::read_to_string(dir.join(WRAPPER_FILE)).unwrap();
    // Arrière-plan classique (Proton Pass) : chargé dans un filet.
    write(r#"{"name":"a","background":{"service_worker":"background.js"}}"#);
    add_compat(&dir).unwrap();
    assert_eq!(manifest()["background"]["service_worker"], WRAPPER_FILE);
    let classic = wrapper();
    let version = format!("// Compléments de Feather {}\n", short_hash(&compat_js()));
    assert!(classic.starts_with(&version));
    assert!(classic.contains("importScripts('/feather-compat.js');\ntry {\n  importScripts(\"/background.js\");\n} catch (e) {"));
    assert!(classic.contains("reportError(e)"));
    // Le verrou de `browser` et `chrome` passe avant les compléments.
    assert_eq!(fs::read_to_string(dir.join(COMPAT_FILE)).unwrap(), compat_js());
    assert!(compat_js().starts_with(LOCK_JS) && compat_js().ends_with(COMPAT_JS));
    assert_eq!(fs::read_to_string(dir.join(LOCK_FILE)).unwrap(), LOCK_JS);
    // Deuxième fois : rien ne change (l'original n'est pas perdu).
    add_compat(&dir).unwrap();
    assert_eq!(wrapper(), classic);
    // Enveloppe des versions précédentes, sans filet : réécrite au chargement suivant.
    fs::write(dir.join(WRAPPER_FILE), "importScripts('/feather-compat.js', \"/background.js\");\n").unwrap();
    add_compat(&dir).unwrap();
    assert_eq!(wrapper(), classic);
    assert_eq!(manifest()["background"]["service_worker"], WRAPPER_FILE);
    // Enveloppe perdue : l'original est introuvable, elle n'est pas réécrite.
    fs::remove_file(dir.join(WRAPPER_FILE)).unwrap();
    assert!(add_compat(&dir).is_err());
    // Module (uBlock Origin Lite).
    write(r#"{"name":"b","background":{"service_worker":"/js/background.js","type":"module"}}"#);
    add_compat(&dir).unwrap();
    let module = format!("{version}import '/feather-compat.js';\nimport \"/js/background.js\";\n");
    assert_eq!(wrapper(), module);
    assert_eq!(manifest()["background"]["type"], "module");
    add_compat(&dir).unwrap();
    assert_eq!(wrapper(), module);
    // Manifeste v2 : les compléments en tête de liste, une seule fois.
    write(r#"{"name":"c","background":{"scripts":["a.js","b.js"]}}"#);
    add_compat(&dir).unwrap();
    add_compat(&dir).unwrap();
    assert_eq!(manifest()["background"]["scripts"], serde_json::json!([COMPAT_FILE, "a.js", "b.js"]));
    // Fenêtre et page d'options : les compléments en premier script, une seule fois.
    write(r#"{"name":"e","action":{"default_popup":"popup.html"},"options_ui":{"page":"opt/options.html?x=1"}}"#);
    fs::write(dir.join("popup.html"), "<!doctype html><html><HEAD lang=\"fr\"><style></style></head><body></body></html>").unwrap();
    fs::create_dir_all(dir.join("opt")).unwrap();
    fs::write(dir.join("opt/options.html"), "<header></header><p>x</p>").unwrap();
    add_compat(&dir).unwrap();
    add_compat(&dir).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("popup.html")).unwrap(),
        format!("<!doctype html><html><HEAD lang=\"fr\">{COMPAT_TAG}<style></style></head><body></body></html>")
    );
    assert_eq!(fs::read_to_string(dir.join("opt/options.html")).unwrap(), format!("{COMPAT_TAG}<header></header><p>x</p>"));
    // Sans arrière-plan : manifeste intact.
    write(r#"{"name":"d"}"#);
    add_compat(&dir).unwrap();
    assert_eq!(manifest(), serde_json::json!({"name": "d"}));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn verrou_dans_les_pages_et_les_scripts_de_contenu() {
    let dir = std::env::temp_dir().join(format!("feather-verrou-{}", stamp()));
    fs::create_dir_all(dir.join("pages/menu")).unwrap();
    // Installation d'une version précédente : arrière-plan déjà enveloppé,
    // scripts de contenu sans le verrou.
    let manifest = r#"{"name":"p","background":{"service_worker":"feather-background.js"},"content_scripts":[
        {"matches":["https://*/*"],"js":["orchestrator.js"],"all_frames":true},
        {"matches":["https://*/*"],"js":["webauthn.js"],"world":"MAIN"},
        {"matches":["https://*/*"],"css":["a.css"]},
        {"matches":["https://*/*"],"js":[]}]}"#;
    fs::write(dir.join("manifest.json"), manifest).unwrap();
    fs::write(dir.join(WRAPPER_FILE), wrapper_js("/background.js", false).unwrap()).unwrap();
    let pages = [
        ("dropdown.html", "<!doctype html><html><head><script src=\"d.js\"></script></head></html>"),
        ("pages/menu/notification.HTM", "<!DOCTYPE html><p>x</p>"),
        ("pages/onboarding.html", "<!doctype html>\n<html lang=\"fr\"><body></body></html>"),
        ("pages/vide.html", "<p>x</p>"),
    ];
    for (file, html) in pages {
        fs::write(dir.join(file), html).unwrap();
    }
    fs::write(dir.join("notes.txt"), "<head>").unwrap();
    // Page illisible (pas en UTF-8) : laissée telle quelle, sans bloquer le reste.
    fs::write(dir.join("pages/latin1.html"), [b'<', b'p', b'>', 0xe9, b'<']).unwrap();
    add_compat(&dir).unwrap();
    let after = read_json(&dir.join("manifest.json")).unwrap();
    add_compat(&dir).unwrap();
    assert_eq!(read_json(&dir.join("manifest.json")).unwrap(), after);
    let scripts = &after["content_scripts"];
    assert_eq!(scripts[0]["js"], serde_json::json!([LOCK_FILE, "orchestrator.js"]));
    assert_eq!(scripts[0]["all_frames"], true);
    assert_eq!(scripts[1]["js"], serde_json::json!(["webauthn.js"]));
    assert!(scripts[2].get("js").is_none());
    assert_eq!(scripts[3]["js"], serde_json::json!([]));
    assert_eq!(after["background"]["service_worker"], WRAPPER_FILE);
    let read = |file: &str| fs::read_to_string(dir.join(file)).unwrap();
    assert_eq!(read("dropdown.html"), format!("<!doctype html><html><head>{COMPAT_TAG}<script src=\"d.js\"></script></head></html>"));
    assert_eq!(read("pages/menu/notification.HTM"), format!("<!DOCTYPE html>{COMPAT_TAG}<p>x</p>"));
    assert_eq!(read("pages/onboarding.html"), format!("<!doctype html>\n<html lang=\"fr\">{COMPAT_TAG}<body></body></html>"));
    assert_eq!(read("pages/vide.html"), format!("{COMPAT_TAG}<p>x</p>"));
    assert_eq!(read("notes.txt"), "<head>");
    assert_eq!(fs::read(dir.join("pages/latin1.html")).unwrap(), [b'<', b'p', b'>', 0xe9, b'<']);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn erreurs_du_moteur_expliquees() {
    let webrequest = "Non-persistent background content cannot listen to `webRequest` events.";
    assert!(explain_error(webrequest).starts_with("WebKit ne laisse pas"));
    assert_eq!(explain_error("Unable to parse manifest."), "Unable to parse manifest.");
}

#[test]
fn original_relu_dans_l_enveloppe() {
    // Toutes les versions de l'enveloppe, et un chemin à échapper.
    assert_eq!(wrapped_original("importScripts('/feather-compat.js', \"/background.js\");\n").as_deref(), Some("/background.js"));
    assert_eq!(wrapped_original("import '/feather-compat.js';\nimport \"/js/bg.js\";\n").as_deref(), Some("/js/bg.js"));
    let odd = "/arrière \"plan\"\\x.js";
    assert_eq!(wrapped_original(&wrapper_js(odd, false).unwrap()).as_deref(), Some(odd));
    assert_eq!(wrapped_original(&wrapper_js(odd, true).unwrap()).as_deref(), Some(odd));
    assert_eq!(wrapped_original("importScripts('/feather-compat.js');\n"), None);
}

#[test]
fn sites_declares_par_les_extensions() {
    // Motifs de Proton Pass ; les motifs trop larges sont ignorés.
    let proton = declared("https://*.proton.me/*").unwrap();
    assert_eq!(proton, Declared { scheme: "https".into(), host: "proton.me".into(), subdomains: true });
    assert_eq!(declared("*://Mail.Exemple.fr:8443/boite/*").map(|d| (d.host, d.subdomains)), Some(("mail.exemple.fr".into(), false)));
    for broad in ["<all_urls>", "*://*/*", "https://*/*", "https://*.com/*", "https://localhost/*", "ftp://a.b/*", "https://*.*.fr/*", "pas un motif"] {
        assert_eq!(declared(broad), None, "{broad}");
    }
    let list = [proton, declared("*://mail.exemple.fr/*").unwrap()];
    for url in ["https://account.proton.me/authorize?app=proton-pass-extension", "https://proton.me/", "https://PASS.proton.me/", "http://mail.exemple.fr/", "https://mail.exemple.fr/x"] {
        assert!(site_matches(&list, url), "{url}");
    }
    for url in ["http://account.proton.me/", "https://evilproton.me/", "https://proton.me.exemple.org/", "https://a.mail.exemple.fr/", "navigateur://accueil/", "pas une adresse"] {
        assert!(!site_matches(&list, url), "{url}");
    }
}

#[test]
fn place_de_la_fenetre() {
    let win = (1000.0, 700.0);
    // Barre à gauche : à droite du bouton.
    assert_eq!(popup_frame((10.0, 100.0, 30.0, 26.0), (300.0, 400.0), win, "left"), (48.0, 100.0, 300.0, 400.0));
    // Barre à droite : à gauche du bouton.
    assert_eq!(popup_frame((960.0, 100.0, 30.0, 26.0), (300.0, 400.0), win, "right").0, 652.0);
    // En haut : sous le bouton, sans sortir de la fenêtre.
    assert_eq!(popup_frame((980.0, 9.0, 26.0, 26.0), (300.0, 400.0), win, "top"), (692.0, 43.0, 300.0, 400.0));
    // Trop grande : ramenée à 800×600 au plus, et dans la fenêtre.
    let (x, y, w, h) = popup_frame((10.0, 600.0, 30.0, 26.0), (5000.0, 5000.0), win, "left");
    assert_eq!((w, h), (800.0, 600.0));
    assert!(x + w <= 992.0 && y + h <= 692.0);
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Un zip sans compression (les cas difficiles sont dans les tests d'`archive`).
fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let head = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend(20u16.to_le_bytes());
            }
            for v in [20u16, 0, 0, 0, 0x21] {
                h.extend(v.to_le_bytes());
            }
            h.extend(crc.to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            if central {
                for v in [0u16, 0, 0] {
                    h.extend(v.to_le_bytes());
                }
                h.extend(0u32.to_le_bytes());
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(head(0x0403_4b50, false));
        out.extend(*data);
        central.extend(head(0x0201_4b50, true));
    }
    let start = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    for v in [0u16, 0, files.len() as u16, files.len() as u16] {
        out.extend(v.to_le_bytes());
    }
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// De bout en bout : un `.crx` (version 3) choisi comme fichier est vérifié,
/// décompressé, rangé, et reçoit sa clé.
#[test]
fn fichier_crx_installe() {
    let crx = base64_decode(SIGNED_CRX).unwrap();
    let ext = prepare(Request::File { name: "essai.crx".into(), bytes: crx }).unwrap();
    assert_eq!(ext.id, SIGNED_ID);
    assert_eq!(ext.name, "Essai");
    assert_eq!(ext.source, Source::Fichier);
    assert_eq!(ext.popup.as_deref(), Some("popup.html"));
    assert_eq!(ext.icon.as_deref(), Some("icons/32.png"));
    assert!(ext.dir.ends_with("/essai"));
    let path = ext.path().unwrap();
    assert_eq!(fs::read_to_string(path.join("popup.html")).unwrap(), "<p>bonjour</p>");
    assert!(read_manifest(&path).unwrap().has_key);
    assert!(icon_data(&ext).unwrap().starts_with("data:image/png;base64,"));
    fs::remove_dir_all(root().unwrap().join(ext.dir.split('/').next().unwrap())).unwrap();

    // Pas une extension : refusé, sans rien laisser derrière.
    assert!(prepare(Request::File { name: "x.zip".into(), bytes: zip(&[("a.txt", b"a")]) }).is_err());
    let left = fs::read_dir(root().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".nouvelle")).count();
    assert_eq!(left, 0);
}
