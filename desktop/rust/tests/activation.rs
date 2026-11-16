//! CP2 S3 portable activation evidence (plan §6.1): link grammar, one-shot activation slot,
//! WM_COPYDATA header decision table with an instrumented reader, and the Deliver page bytes.
//! Linux/portable only: nothing here proves Windows message delivery, registry behaviour,
//! browser prompts, focus rules or a real Google login (NATIVE-GATE, user-owned).
use nimbo_desktop::login::*;
use nimbo_desktop::loopback::{
    ACTIVATION_PAGE_CSP, ACTIVATION_SCRIPT, LOGIN_DONE_PAGE, Response, SecretBuf, response_bytes,
};
use nimbo_desktop::shell::{
    CopyDataPlan, FOCUS_REQUEST, InstanceRequest, LINK_REQUEST, MAX_LINK_BYTES, MIN_LINK_BYTES,
    instance_request, plan_copydata, test_host_hash,
};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn iso() -> Scheme {
    Scheme::TestHost(test_host_hash(b"/scratch/activation"))
}
fn iso_name() -> String {
    format!(
        "golive-testhost-{:016x}",
        test_host_hash(b"/scratch/activation")
    )
}
fn release(raw: &str) -> Result<ActivationLink, LinkError> {
    ActivationLink::parse(raw, Scheme::Release)
}

#[test]
fn activation_link_grammar_matrix() {
    for ok in [
        format!("golive://auth?state={HEX}"),
        format!("GOLIVE://auth?state={HEX}"),
        format!("GoLive://auth?state={HEX}"),
        format!("golive://auth/?state={HEX}"),
    ] {
        assert!(release(&ok).is_ok(), "accept {ok}");
    }
    let name = iso_name();
    for ok in [
        format!("{name}://auth?state={HEX}"),
        format!("{}://auth/?state={HEX}", name.to_uppercase()),
    ] {
        assert!(
            ActivationLink::parse(&ok, iso()).is_ok(),
            "accept isolated {ok}"
        );
    }
    let long = format!("golive://auth?state={HEX}{}", "a".repeat(257 - 68));
    assert_eq!(long.chars().count(), 257);
    let rejected = [
        format!("golive://auth?state={}", &HEX[..47]),
        format!("golive://auth?state={HEX}a"),
        format!("golive://auth?state={}", HEX.to_uppercase()),
        format!("golive://auth?state=A{}", &HEX[1..]),
        "golive://auth".to_owned(),
        "golive://auth?".to_owned(),
        "golive://auth?state=".to_owned(),
        format!("golive://auth?state={HEX}&state={HEX}"),
        format!("golive://auth?token={HEX}"),
        format!("golive://auth?x=1&state={HEX}"),
        format!("golive://auth?state={HEX}&x=1"),
        format!("golive://auth?state={HEX}#frag"),
        format!("golive://user@auth?state={HEX}"),
        format!("golive://auth//?state={HEX}"),
        format!("golive://auth/complete?state={HEX}"),
        format!("golive://AUTH?state={HEX}"),
        format!("golive://auth?State={HEX}"),
        format!(" golive://auth?state={HEX}"),
        format!("golive://auth?state={HEX} "),
        format!("golive://auth?state={HEX}\r"),
        format!("golive://auth?state={HEX}\n"),
        format!("golive://auth?state={HEX}\0"),
        format!("golive:\\\\auth?state={HEX}"),
        format!("golive://auth\\?state={HEX}"),
        format!("golive://auth?state=%61{}", &HEX[1..]),
        format!("golive://%61uth?state={HEX}"),
        format!("nimbo://auth?state={HEX}"),
        format!("golive:auth?state={HEX}"),
        format!("golive:/auth?state={HEX}"),
        format!("golivex://auth?state={HEX}"),
        long,
        String::new(),
        format!("{name}://auth?state={HEX}"),
    ];
    for bad in &rejected {
        assert_eq!(release(bad), Err(LinkError), "reject {bad:?}");
    }
    // Isolated scheme: the release link (and another directory's scheme) are rejected.
    let other = Scheme::TestHost(test_host_hash(b"/scratch/other"));
    for bad in [
        format!("golive://auth?state={HEX}"),
        format!("{}://auth?state={HEX}", other.name()),
        format!("{name}x://auth?state={HEX}"),
    ] {
        assert_eq!(
            ActivationLink::parse(&bad, iso()),
            Err(LinkError),
            "reject {bad}"
        );
    }
}

#[test]
fn activation_link_debug_and_canonical_exact() {
    for raw in [
        format!("golive://auth?state={HEX}"),
        format!("GOLIVE://auth/?state={HEX}"),
    ] {
        let link = release(&raw).unwrap();
        assert!(
            !format!("{link:?}").contains(&HEX[..8]),
            "Debug carries no state"
        );
        assert_eq!(format!("{link:?}"), "ActivationLink(..)");
        assert_eq!(
            link.canonical(),
            format!("{}://auth?state={}", "golive", HEX),
            "exact canonical string"
        );
        assert_eq!(link.scheme(), Scheme::Release);
        assert_eq!(release(&link.canonical()), Ok(link));
    }
    let name = iso_name();
    let link = ActivationLink::parse(
        &format!("{}://auth/?state={HEX}", name.to_uppercase()),
        iso(),
    )
    .unwrap();
    assert_eq!(link.canonical(), format!("{name}://auth?state={HEX}"));
    assert_eq!(ActivationLink::parse(&link.canonical(), iso()), Ok(link));
}

#[test]
fn utf16le_round_trip_and_bound() {
    let name = iso_name();
    // Actual grammar maxima of the fixed identifiers (plan §16): release 68/69 and isolated
    // 94/95 characters without/with the optional slash; canonical output omits the slash.
    let cases = [
        (format!("golive://auth?state={HEX}"), Scheme::Release, 68),
        (format!("golive://auth/?state={HEX}"), Scheme::Release, 69),
        (format!("{name}://auth?state={HEX}"), iso(), 94),
        (format!("{name}://auth/?state={HEX}"), iso(), 95),
    ];
    for (raw, scheme, len) in cases {
        assert_eq!(raw.chars().count(), len);
        let link = ActivationLink::parse(&raw, scheme).unwrap();
        let bytes = link.utf16le();
        assert_eq!(bytes.len(), 2 * link.canonical().len());
        assert!(bytes.len() <= 188 && bytes.len() <= 512);
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes(*p))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), link.canonical());
    }
    assert_eq!(MAX_LINK_CHARS, 256);
}

// ---------- one-shot activation slot (A3/A4) ----------

/// A real flow whose rng yields a fresh byte pattern per login and whose spawner drops the
/// acceptor job (no receiver thread: these rows exercise the state machine only).
fn flow(scheme: Scheme) -> LoginFlow {
    let seed = Arc::new(AtomicU8::new(1));
    let mut hooks = Hooks::new(
        Arc::new(move |buf: &mut [u8; 24]| {
            let b = seed.fetch_add(1, Ordering::SeqCst);
            for (i, x) in buf.iter_mut().enumerate() {
                *x = b.wrapping_mul(31).wrapping_add(i as u8);
            }
            true
        }),
        Arc::new(|| true),
        Arc::new(|| 0),
    );
    hooks.spawn = Arc::new(|job| {
        drop(job);
        Ok(())
    });
    LoginFlow::new(hooks, scheme)
}
const CALLER: Caller = Caller {
    generation: 1,
    id: 9,
};
/// begin + Deliver at `now`; returns the delivered state (48 hex).
fn deliver(flow: &LoginFlow, now: u64) -> String {
    let begun = flow.begin(CALLER, "http://127.0.0.1:4173", now);
    let url = begun.started.expect("begin");
    let state = url.as_str().split("state=").nth(1).unwrap().to_owned();
    let id = flow.current_login_id().unwrap();
    let credential = Credential::new(
        SecretBuf::new(b"synthetic-id".to_vec()),
        SecretBuf::new(b"synthetic-access".to_vec()),
    );
    assert_eq!(
        flow.callback(id, credential, now),
        CallbackDecision::Deliver
    );
    state
}
fn link_for(scheme: Scheme, state: &str) -> ActivationLink {
    ActivationLink::parse(&format!("{}://auth?state={state}", scheme.name()), scheme).unwrap()
}
fn forged(state: &str) -> String {
    let flip = if state.starts_with('a') { "b" } else { "a" };
    format!("{flip}{}", &state[1..])
}

#[test]
fn activate_consumes_once_and_only_on_exact_match() {
    for claim_first in [false, true] {
        let f = flow(iso());
        let state = deliver(&f, 10);
        assert!(f.activation_reserved());
        if claim_first {
            assert_eq!(
                f.take_settlements(11).len(),
                1,
                "credential claimed before activation"
            );
        }
        let active = f.is_active();
        // Wrong state, and the RIGHT state under another scheme: ignored, slot untouched.
        assert_eq!(
            f.activate(&link_for(iso(), &forged(&state)), 11),
            Activation::Ignored
        );
        assert!(
            f.activation_reserved(),
            "a forged link never consumes the slot"
        );
        assert_eq!(
            f.activate(&link_for(Scheme::Release, &state), 11),
            Activation::Ignored,
            "matching state with the wrong scheme"
        );
        assert!(f.activation_reserved());
        assert_eq!(
            f.activate(&link_for(iso(), &state), 12),
            Activation::Activated
        );
        assert!(!f.activation_reserved(), "slot consumed");
        assert_eq!(
            f.is_active(),
            active,
            "activation never changes the login state"
        );
        assert_eq!(
            f.activate(&link_for(iso(), &state), 13),
            Activation::Ignored,
            "replay"
        );
        let s = f.take_settlements(14);
        assert_eq!(
            s.len(),
            usize::from(!claim_first),
            "exactly one credential overall"
        );
        if let Some(settle) = s.first() {
            assert!(matches!(settle.outcome, Outcome::Credential(_)));
        }
    }
}

#[test]
fn activate_respects_ttl_new_login_cancel_and_shutdown() {
    // TTL: the deadline itself is valid, one ms past it is expired (and cleared).
    let f = flow(iso());
    let state = deliver(&f, 100);
    let _ = f.take_codes();
    assert_eq!(
        f.activate(&link_for(iso(), &state), 100 + ACTIVATION_MS),
        Activation::Activated
    );
    let f = flow(iso());
    let state = deliver(&f, 100);
    let _ = f.take_codes();
    assert_eq!(
        f.activate(&link_for(iso(), &state), 101 + ACTIVATION_MS),
        Activation::Ignored
    );
    assert!(!f.activation_reserved(), "expired slot cleared");
    assert!(f.take_codes().0.contains(&"activation-expired"));
    // A new login clears the old slot.
    let f = flow(iso());
    let old = deliver(&f, 10);
    let _ = f.take_settlements(11);
    let fresh = f.begin(CALLER, "http://127.0.0.1:4173", 12);
    assert!(fresh.started.is_ok());
    assert_eq!(f.activate(&link_for(iso(), &old), 13), Activation::Ignored);
    // Cancel and shutdown clear it.
    let f = flow(iso());
    let state = deliver(&f, 10);
    let _ = f.cancel(11);
    assert_eq!(
        f.activate(&link_for(iso(), &state), 12),
        Activation::Ignored
    );
    let f = flow(iso());
    let state = deliver(&f, 10);
    let _ = f.shutdown();
    assert_eq!(
        f.activate(&link_for(iso(), &state), 12),
        Activation::Ignored
    );
    // A fresh flow has no slot.
    let f = flow(iso());
    assert_eq!(f.activate(&link_for(iso(), HEX), 1), Activation::Ignored);
    assert_eq!(f.scheme(), iso());
}

// ---------- WM_COPYDATA header table + instrumented reader (A5) ----------

/// Applies `plan` with a recording reader that serves `bytes` (or `None`) and returns the
/// result plus every length the reader was asked for.
fn apply(
    plan: CopyDataPlan,
    scheme: Scheme,
    serve: Option<Vec<u8>>,
) -> (Option<InstanceRequest>, Vec<usize>) {
    let calls = RefCell::new(Vec::new());
    let out = instance_request(plan, scheme, |len| {
        calls.borrow_mut().push(len);
        serve.clone()
    });
    (out, calls.into_inner())
}
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn copydata_plan_is_total_and_reader_is_called_only_for_bounded_links() {
    use CopyDataPlan::*;
    let mut rows: Vec<(usize, u32, bool, CopyDataPlan)> = vec![
        (FOCUS_REQUEST, 0, true, Focus),
        (FOCUS_REQUEST, 0, false, Focus),
        (FOCUS_REQUEST, 1, false, Reject),
        (FOCUS_REQUEST, 2, false, Reject),
        (FOCUS_REQUEST, 2, true, Reject),
        (FOCUS_REQUEST, u32::MAX, false, Reject),
        (LINK_REQUEST, 0, false, Reject),
        (LINK_REQUEST, 0, true, Reject),
        (LINK_REQUEST, 1, false, Reject),
        (LINK_REQUEST, 3, false, Reject),
        (LINK_REQUEST, 511, false, Reject),
        (LINK_REQUEST, 513, false, Reject),
        (LINK_REQUEST, 514, false, Reject),
        (LINK_REQUEST, 4096, false, Reject),
        (LINK_REQUEST, u32::MAX, false, Reject),
        (LINK_REQUEST, u32::MAX - 1, false, Reject),
        (LINK_REQUEST, 2, true, Reject),
        (LINK_REQUEST, 188, true, Reject),
        (LINK_REQUEST, 512, true, Reject),
        (LINK_REQUEST, 2, false, CopyLink { len: 2 }),
        (LINK_REQUEST, 136, false, CopyLink { len: 136 }),
        (LINK_REQUEST, 188, false, CopyLink { len: 188 }),
        (LINK_REQUEST, 510, false, CopyLink { len: 510 }),
        (LINK_REQUEST, 512, false, CopyLink { len: 512 }),
    ];
    for magic in [
        0,
        FOCUS_REQUEST + 1,
        LINK_REQUEST + 1,
        LINK_REQUEST - 1,
        usize::MAX,
    ] {
        for len in [0, 2, 188, u32::MAX] {
            for null in [false, true] {
                rows.push((magic, len, null, Reject));
            }
        }
    }
    let link =
        ActivationLink::parse(&format!("golive://auth?state={HEX}"), Scheme::Release).unwrap();
    for (data, len, null, want) in rows {
        let plan = plan_copydata(data, len, null);
        assert_eq!(plan, want, "plan {data:#x} {len} null={null}");
        let (out, calls) = apply(plan, Scheme::Release, Some(link.utf16le()));
        match plan {
            Focus => {
                assert_eq!(out, Some(InstanceRequest::Focus));
                assert!(calls.is_empty(), "Focus never reads lpData");
            }
            Reject => {
                assert_eq!(out, None);
                assert!(
                    calls.is_empty(),
                    "Reject never reads lpData ({data:#x} {len})"
                );
            }
            CopyLink { len: n } => {
                assert_eq!(n, len as usize);
                assert_eq!(
                    calls,
                    vec![len as usize],
                    "exactly one read of cbData bytes"
                );
            }
        }
    }
    assert_eq!((MIN_LINK_BYTES, MAX_LINK_BYTES), (2, 512));

    // Valid canonical links (release and isolated) decode to Link.
    for (scheme, raw) in [
        (Scheme::Release, format!("golive://auth?state={HEX}")),
        (iso(), format!("{}://auth?state={HEX}", iso_name())),
    ] {
        let bytes = utf16(&raw);
        let plan = plan_copydata(LINK_REQUEST, bytes.len() as u32, false);
        let (out, calls) = apply(plan, scheme, Some(bytes.clone()));
        let expected = ActivationLink::parse(&raw, scheme).unwrap();
        assert_eq!(out, Some(InstanceRequest::Link(expected)));
        assert_eq!(calls, vec![bytes.len()]);
        assert!(
            !format!("{out:?}").contains(&HEX[..8]),
            "Debug carries no state"
        );
        // The other scheme rejects it after the (single) read.
        let other = if scheme == Scheme::Release {
            iso()
        } else {
            Scheme::Release
        };
        let (out, calls) = apply(plan, other, Some(bytes));
        assert_eq!((out, calls.len()), (None, 1));
    }

    // Reader failures and malformed payloads: read once, then None.
    let good = utf16(&format!("golive://auth?state={HEX}"));
    let plan = plan_copydata(LINK_REQUEST, good.len() as u32, false);
    let mut nul = good.clone();
    nul.extend_from_slice(&[0, 0]);
    let mut bom = vec![0xff, 0xfe];
    bom.extend_from_slice(&good[..good.len() - 2]);
    let mut surrogate = good[..good.len() - 2].to_vec();
    surrogate.extend_from_slice(&0xd800u16.to_le_bytes());
    let mut embedded = good.clone();
    embedded[20] = 0;
    embedded[21] = 0;
    let upper = utf16(&format!("golive://auth?state={}", HEX.to_uppercase()));
    let text = utf16(&"x".repeat(good.len() / 2));
    let cases: Vec<(&str, CopyDataPlan, Option<Vec<u8>>)> = vec![
        ("reader None", plan, None),
        ("short read", plan, Some(good[..good.len() - 2].to_vec())),
        ("long read", plan, Some(nul.clone())),
        (
            "trailing NUL",
            plan_copydata(LINK_REQUEST, nul.len() as u32, false),
            Some(nul),
        ),
        ("BOM prefix", plan, Some(bom)),
        ("unpaired surrogate", plan, Some(surrogate)),
        ("embedded NUL", plan, Some(embedded)),
        ("uppercase hex", plan, Some(upper)),
        ("non-link text", plan, Some(text)),
    ];
    for (what, plan, serve) in cases {
        let (out, calls) = apply(plan, Scheme::Release, serve);
        assert_eq!(out, None, "{what}");
        assert_eq!(
            calls.len(),
            1,
            "{what}: read happened once, grammar rejected"
        );
    }
}

// ---------- Deliver page bytes (A7, pure) ----------

#[test]
fn response_for_deliver_is_the_activation_page_only() {
    for (scheme, raw) in [
        (iso(), format!("{}://auth?state={HEX}", iso_name())),
        (Scheme::Release, format!("golive://auth?state={HEX}")),
    ] {
        let link = ActivationLink::parse(&raw, scheme).unwrap();
        let bytes = response_bytes(Response::Activate(link));
        assert!(bytes.len() <= 4096, "response {} bytes", bytes.len());
        let text = String::from_utf8(bytes).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
        for header in [
            "Content-Type: text/html; charset=utf-8".to_owned(),
            format!("Content-Length: {}", body.len()),
            "Cache-Control: no-store".to_owned(),
            "Connection: close".to_owned(),
            format!("Content-Security-Policy: {ACTIVATION_PAGE_CSP}"),
            "Referrer-Policy: no-referrer".to_owned(),
        ] {
            assert!(head.split("\r\n").any(|h| h == header), "header {header}");
        }
        assert!(
            ACTIVATION_PAGE_CSP
                .starts_with("default-src 'none'; style-src 'unsafe-inline'; script-src 'sha256-")
        );
        assert_eq!(text.matches(HEX).count(), 1, "state exactly once");
        assert!(
            body.contains(&format!(
                "<a id=\"open\" href=\"{}\" rel=\"noreferrer\">Abrir o app</a>",
                link.canonical()
            )),
            "state only inside the anchor href"
        );
        for sentence in [
            "<h1>Pronto, você entrou.</h1>",
            "<p>Pode fechar esta aba e voltar ao app Nimbo.</p>",
            "Se o Nimbo não voltou para a frente, clique em Abrir o app.",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
            "<title>Nimbo</title>",
        ] {
            assert!(body.contains(sentence), "{sentence}");
        }
        assert_eq!(body.matches("<script>").count(), 1);
        assert!(body.contains(&format!("<script>{ACTIVATION_SCRIPT}</script>")));
        let outside = body.replace(&link.canonical(), "");
        for forbidden in [
            "http",
            "src=",
            "@import",
            "url(",
            "synthetic",
            "<link",
            "<img",
            "<form",
        ] {
            assert!(
                !outside.contains(forbidden),
                "no {forbidden} outside the href"
            );
        }
    }
    // Positive control: the plain page stays byte-identical.
    let done = String::from_utf8(response_bytes(Response::Done)).unwrap();
    assert!(done.ends_with(LOGIN_DONE_PAGE));
    assert!(!done.contains("Content-Security-Policy") && !done.contains("Abrir o app"));
}
