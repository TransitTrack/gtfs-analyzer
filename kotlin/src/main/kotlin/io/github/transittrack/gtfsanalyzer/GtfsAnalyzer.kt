package io.github.transittrack.gtfsanalyzer

import kotlinx.datetime.LocalDate
import kotlinx.datetime.TimeZone
import kotlinx.datetime.format
import kotlinx.datetime.todayIn
import kotlinx.io.buffered
import kotlinx.io.files.Path
import kotlinx.io.files.SystemFileSystem
import kotlinx.io.readByteArray
import kotlin.time.Clock

/**
 * The SDK's sole public entry point: validates a GTFS feed using the shared Rust validation
 * engine (the same engine backing the CLI and WASM/TS SDKs) and returns its decoded result.
 */
object GtfsAnalyzer {
    init {
        loadNativeLibrary()
    }
    /**
     * Validates a GTFS feed already loaded into memory as a ZIP byte buffer.
     *
     * Blocking — runs the validation engine on the calling thread. Typically completes in well
     * under a second; callers on a coroutine dispatcher should wrap this with
     * `withContext(Dispatchers.IO)`.
     *
     * Loads the bundled native library on first use (see [loadNativeLibrary]), which throws
     * [UnsupportedPlatformException] or [NativeLibraryLoadException] if that fails. Throws
     * [GtfsEngineException] if the native engine panics, or `IllegalArgumentException` if
     * [config] or [lang] is rejected by the engine.
     */
    fun validate(
        feed: ByteArray,
        today: LocalDate = Clock.System.todayIn(TimeZone.currentSystemDefault()),
        config: ValidatorConfigDelta = ValidatorConfigDelta(),
        includeNameIndex: Boolean = false,
        lang: String = "en",
    ): EngineResult {
        val todayInt = today.format(LocalDate.Formats.ISO_BASIC).toInt() // YYYYMMDD
        val configJson = EngineJson.encodeToString(config)
        val json = GtfsAnalyzerNative.validate(feed, todayInt, configJson, includeNameIndex, lang)
        return EngineJson.decodeFromString(json)
    }

    /**
     * Validates a GTFS feed read from disk.
     *
     * Blocking — runs the validation engine on the calling thread (including the synchronous
     * read of [feedPath]). Typically completes in well under a second; callers on a coroutine
     * dispatcher should wrap this with `withContext(Dispatchers.IO)`.
     *
     * Throws `kotlinx.io.IOException` (including `kotlinx.io.files.FileNotFoundException`) if [feedPath] cannot be
     * read, [UnsupportedPlatformException] or [NativeLibraryLoadException] if the bundled native
     * library can't be loaded, [GtfsEngineException] if the native engine panics, or
     * `IllegalArgumentException` if [config] or [lang] is rejected by the engine.
     */
    fun validate(
        feedPath: Path,
        today: LocalDate = Clock.System.todayIn(TimeZone.currentSystemDefault()),
        config: ValidatorConfigDelta = ValidatorConfigDelta(),
        includeNameIndex: Boolean = false,
        lang: String = "en",
    ): EngineResult {
        val feed = SystemFileSystem.source(feedPath).buffered().use { it.readByteArray() }
        return validate(feed, today, config, includeNameIndex, lang)
    }
}
