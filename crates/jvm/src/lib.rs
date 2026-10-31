use gtfs_config::{merge_delta, ValidatorConfig};
use gtfs_core::i18n::{Lang, Translator};
use gtfs_core::{NameIndex, ValidateResult};
use gtfs_pipeline::validate_bytes;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JString};
use jni::strings::{JNIStr, JNIString};
use jni::sys::{jboolean, jint};
use jni::{jni_str, Env, EnvUnowned};

/// Validate one GTFS ZIP buffer using the shared Rust validation pipeline.
pub fn validate_to_json(
    feed: &[u8],
    today: u32,
    config_json: &str,
    include_name_index: bool,
    lang: &str,
) -> Result<String, String> {
    if std::env::var_os("GTFS_QUIET").is_none() {
        std::env::set_var("GTFS_QUIET", "1");
    }

    // Lang is checked BEFORE validation: an invalid code should fail immediately,
    // not after the feed has been processed.
    let lang: Lang = lang.parse()?;
    let translator = Translator::new(lang)?;
    let config = merge_delta(&ValidatorConfig::default(), config_json)?;
    let mut result = validate_bytes(feed, &config, today);

    // Same translation as the CLI (crates/cli/src/main.rs): title/message/remediation and fatal.
    if let Some(translator) = &translator {
        match &mut result {
            ValidateResult::Ok(validation) => {
                for notice in &mut validation.notices {
                    translator.translate(notice);
                }
            }
            ValidateResult::Fatal(err) => translator.translate_fatal(err),
        }
    }

    if !include_name_index {
        if let ValidateResult::Ok(validation) = &mut result {
            validation.name_index = NameIndex::default();
        }
    }

    serde_json::to_string(&result).map_err(|e| e.to_string())
}

#[derive(Debug)]
pub enum ValidateJvmOutcome {
    Json(String),
    InvalidArgument(String),
    Panicked(String),
}

fn catch_unwind_to_outcome<F>(f: F) -> ValidateJvmOutcome
where
    F: FnOnce() -> Result<String, String> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(json)) => ValidateJvmOutcome::Json(json),
        Ok(Err(msg)) => ValidateJvmOutcome::InvalidArgument(msg),
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic payload".to_string());
            ValidateJvmOutcome::Panicked(msg)
        }
    }
}

pub fn validate_guarded(
    feed: &[u8],
    today: u32,
    config_json: &str,
    include_name_index: bool,
    lang: &str,
) -> ValidateJvmOutcome {
    catch_unwind_to_outcome(|| validate_to_json(feed, today, config_json, include_name_index, lang))
}

/// Throws a new `class` exception with `msg` and returns the null `JString` that JNI requires
/// as this native method's return value while an exception is pending.
///
/// The `throw_new` result is ignored: if throwing itself fails (e.g. the class can't be found),
/// the JVM has already set a different pending exception, which is just as good for the caller.
/// No further JNI call is made afterwards — the JNI spec forbids most calls while an exception
/// is pending — and the null `JString` is built without touching the JVM at all.
fn throw_and_return_null<'local>(
    env: &mut Env<'local>,
    class: &JNIStr,
    msg: impl AsRef<str>,
) -> jni::errors::Result<JString<'local>> {
    let _ = env.throw_new(class, JNIString::new(msg));
    Ok(JString::default())
}

/// JNI entry point for `GtfsAnalyzerNative.validate(...)` on the Kotlin side.
///
/// Converts the JNI arguments to Rust types, dispatches to [`validate_guarded`],
/// and converts the outcome back across the JNI boundary:
/// - `Json(s)` returns a new Java string containing `s`.
/// - `InvalidArgument(msg)` throws `java.lang.IllegalArgumentException` with `msg`.
/// - `Panicked(msg)` throws `io/github/transittrack/gtfsanalyzer/GtfsEngineException` with `msg`.
///
/// Everything runs inside [`EnvUnowned::with_env`], which also catches any panic outside
/// [`validate_guarded`] so it can never unwind into the JVM. Any `Err` that escapes the closure
/// (e.g. `JString::from_str` failing on JVM heap exhaustion for a very large result, realistic
/// when `includeNameIndex=true`) is resolved by [`ThrowRuntimeExAndDefault`]: if the failed JNI
/// call already left an exception pending it is propagated as-is, otherwise a
/// `java.lang.RuntimeException` is thrown; either way a null `JString` is returned.
#[no_mangle]
pub extern "system" fn Java_io_github_transittrack_gtfsanalyzer_GtfsAnalyzerNative_validate<
    'local,
>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    feed: JByteArray<'local>,
    today: jint,
    config_json: JString<'local>,
    include_name_index: jboolean,
    lang: JString<'local>,
) -> JString<'local> {
    env.with_env(|env| -> jni::errors::Result<JString<'local>> {
        const ILLEGAL_ARGUMENT: &JNIStr = jni_str!("java/lang/IllegalArgumentException");

        let feed_bytes = match env.convert_byte_array(&feed) {
            Ok(bytes) => bytes,
            Err(e) => {
                return throw_and_return_null(
                    env,
                    ILLEGAL_ARGUMENT,
                    format!("invalid feed byte array: {e}"),
                )
            }
        };

        let config_json_str = match config_json.try_to_string(env) {
            Ok(s) => s,
            Err(e) => {
                return throw_and_return_null(
                    env,
                    ILLEGAL_ARGUMENT,
                    format!("invalid config_json string: {e}"),
                )
            }
        };

        let lang_str = match lang.try_to_string(env) {
            Ok(s) => s,
            Err(e) => {
                return throw_and_return_null(
                    env,
                    ILLEGAL_ARGUMENT,
                    format!("invalid lang string: {e}"),
                )
            }
        };

        match validate_guarded(
            &feed_bytes,
            today as u32,
            &config_json_str,
            include_name_index,
            &lang_str,
        ) {
            ValidateJvmOutcome::Json(s) => JString::from_str(env, s),
            ValidateJvmOutcome::InvalidArgument(msg) => {
                throw_and_return_null(env, ILLEGAL_ARGUMENT, msg)
            }
            ValidateJvmOutcome::Panicked(msg) => throw_and_return_null(
                env,
                jni_str!("io/github/transittrack/gtfsanalyzer/GtfsEngineException"),
                msg,
            ),
        }
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;

    // Same fixed date used across tests — deterministic analytics output.
    const TODAY: u32 = 20_260_515;

    // ── Helpers ─────────────────────────────────────────────────────────

    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let cur = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(cur);
        let opts = SimpleFileOptions::default();
        for (name, data) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    // Date range (20250101-20271231) covers the test day (20260515, Friday).
    // Monday-Friday=1 -> SVC1 active -> no service-gap notice.
    static AGENCY: &[u8] =
        b"agency_id,agency_name,agency_url,agency_timezone\n1,Test,http://test.example,UTC\n";
    static STOPS: &[u8] =
        b"stop_id,stop_name,stop_lat,stop_lon\nS1,Stop1,41.0,29.0\nS2,Stop2,41.1,29.1\n";
    static ROUTES: &[u8] = b"route_id,agency_id,route_short_name,route_type\nR1,1,101,3\n";
    static TRIPS: &[u8] = b"route_id,service_id,trip_id\nR1,SVC1,T1\n";
    static STOP_TIMES: &[u8] = b"trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
          T1,08:00:00,08:00:00,S1,1\nT1,08:10:00,08:10:00,S2,2\n";
    static CALENDAR: &[u8] =
        b"service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
          SVC1,1,1,1,1,1,0,0,20250101,20271231\n";

    fn base_files() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("agency.txt", AGENCY),
            ("stops.txt", STOPS),
            ("routes.txt", ROUTES),
            ("trips.txt", TRIPS),
            ("stop_times.txt", STOP_TIMES),
            ("calendar.txt", CALENDAR),
        ]
    }

    #[test]
    fn validate_to_json_returns_ok_envelope_for_a_clean_feed() {
        let zip = make_zip(&base_files());
        let json = validate_to_json(&zip, TODAY, "{}", false, "en").unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            value.get("Ok").is_some(),
            "expected an {{\"Ok\": ...}} envelope, got: {json}"
        );
        let metrics = &value["Ok"]["metrics"];
        assert_eq!(metrics["stop_count"], 2);
        assert_eq!(metrics["route_count"], 1);
        assert_eq!(metrics["trip_count"], 1);
    }

    #[test]
    fn validate_to_json_rejects_unknown_config_key() {
        let zip = make_zip(&base_files());
        let err =
            validate_to_json(&zip, TODAY, r#"{"not_a_real_key": 1}"#, false, "en").unwrap_err();
        assert!(
            err.contains("not_a_real_key"),
            "error should name the bad key: {err}"
        );
    }

    #[test]
    fn validate_to_json_rejects_unknown_lang() {
        let zip = make_zip(&base_files());
        let err = validate_to_json(&zip, TODAY, "{}", false, "xx").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn validate_guarded_returns_json_on_success() {
        let zip = make_zip(&base_files());
        match validate_guarded(&zip, TODAY, "{}", false, "en") {
            ValidateJvmOutcome::Json(_) => {}
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[test]
    fn validate_guarded_returns_invalid_argument_on_bad_config() {
        let zip = make_zip(&base_files());
        match validate_guarded(&zip, TODAY, r#"{"not_a_real_key": 1}"#, false, "en") {
            ValidateJvmOutcome::InvalidArgument(msg) => assert!(msg.contains("not_a_real_key")),
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn catch_unwind_to_outcome_converts_panic_into_panicked_variant() {
        let outcome = catch_unwind_to_outcome(|| -> Result<String, String> {
            panic!("boom");
        });
        match outcome {
            ValidateJvmOutcome::Panicked(msg) => assert!(msg.contains("boom")),
            other => panic!("expected Panicked, got {other:?}"),
        }
    }
}
