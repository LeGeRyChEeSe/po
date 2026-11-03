package dev.po.remote.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.size
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.po.remote.R

/** Po's own colors: the panda's ink and cream, its pink cheeks and red accent. */
object PoColors {
    val Ink = Color(0xFF3A3F52)
    val Cream = Color(0xFFFBFAF5)
    val Pink = Color(0xFFFF9FB8)
    val Red = Color(0xFFE85A71)
    val Moss = Color(0xFF5E9E7A)
    val Amber = Color(0xFFE0A33A)
    val Night = Color(0xFF23262F)
    val NightCard = Color(0xFF2E323D)
}

/** The app's themes: independent of the terminal's (`/theme` on the PC). */
enum class AppTheme(val key: String, val label: Int) {
    Po("po", R.string.theme_po),
    Ocean("ocean", R.string.theme_ocean),
    Forest("forest", R.string.theme_forest),
    Dusk("dusk", R.string.theme_dusk),
    Contrast("contrast", R.string.theme_contrast);

    companion object {
        fun of(key: String?) = entries.firstOrNull { it.key == key } ?: Po
    }
}

/** Light, dark, or as the phone is set. */
enum class Appearance(val key: String, val label: Int) {
    System("system", R.string.appearance_system),
    Light("light", R.string.appearance_light),
    Dark("dark", R.string.appearance_dark);

    companion object {
        fun of(key: String?) = entries.firstOrNull { it.key == key } ?: System
    }
}

/** How much of a turn's work the conversation shows: quiet (one line for reasoning, one line for tools), normal, verbose. */
enum class Verbosity(val key: String, val label: Int, val hint: Int) {
    Quiet("quiet", R.string.verbosity_quiet, R.string.verbosity_quiet_hint),
    Normal("normal", R.string.verbosity_normal, R.string.verbosity_normal_hint),
    Verbose("verbose", R.string.verbosity_verbose, R.string.verbosity_verbose_hint);

    companion object {
        fun of(key: String?) = entries.firstOrNull { it.key == key } ?: Quiet
    }
}

/**
 * What Material's scheme does not say: the background of the work blocks (reasoning, tools,
 * subagents), set apart from the conversation's, and the color of links.
 */
@Immutable
data class PoExtra(val work: Color, val workBorder: Color, val link: Color, val ok: Color, val warn: Color)

val LocalPo = staticCompositionLocalOf { PoExtra(Color(0xFFEDEBE3), Color(0xFFDCD9CE), PoColors.Red, PoColors.Moss, PoColors.Amber) }

private data class Palette(
    val primary: Color,
    val onPrimary: Color,
    val primaryContainer: Color,
    val onPrimaryContainer: Color,
    val background: Color,
    val onBackground: Color,
    val card: Color,
    val muted: Color,
    val secondaryContainer: Color,
    val tertiary: Color,
    val error: Color,
    val work: Color,
    val workBorder: Color,
    val link: Color,
)

private fun palette(theme: AppTheme, dark: Boolean): Palette = when (theme) {
    AppTheme.Po -> if (dark) Palette(
        PoColors.Pink, PoColors.Ink, Color(0xFF5A2E39), Color(0xFFFFE1E7), PoColors.Night, Color(0xFFE9E8E3),
        PoColors.NightCard, Color(0xFFA9ADBB), PoColors.NightCard, Color(0xFF8CCBA6), Color(0xFFFF8A80),
        Color(0xFF30343F), Color(0xFF3D4250), Color(0xFFFFA9C0),
    ) else Palette(
        PoColors.Red, Color.White, Color(0xFFFFE1E7), PoColors.Ink, PoColors.Cream, PoColors.Ink,
        Color(0xFFF1EFE8), Color(0xFF5D6274), Color(0xFFE8E9EE), PoColors.Moss, Color(0xFFC0392B),
        Color(0xFFEDEBE3), Color(0xFFDCD9CE), Color(0xFFD0435C),
    )
    AppTheme.Ocean -> if (dark) Palette(
        Color(0xFF6FD3FF), Color(0xFF00344A), Color(0xFF004C6B), Color(0xFFC6EAFF), Color(0xFF0E1A24), Color(0xFFDCE7EF),
        Color(0xFF16263A), Color(0xFF9DB2C3), Color(0xFF16263A), Color(0xFF5FE0C8), Color(0xFFFF8A80),
        Color(0xFF162C40), Color(0xFF22405A), Color(0xFF7FDBFF),
    ) else Palette(
        Color(0xFF0277BD), Color.White, Color(0xFFD4ECFB), Color(0xFF00344A), Color(0xFFF4F9FC), Color(0xFF14303F),
        Color(0xFFE6F0F6), Color(0xFF4F6878), Color(0xFFDDEAF2), Color(0xFF00897B), Color(0xFFC62828),
        Color(0xFFE3EEF5), Color(0xFFC9DCE8), Color(0xFF0277BD),
    )
    AppTheme.Forest -> if (dark) Palette(
        Color(0xFF9CD67D), Color(0xFF173807), Color(0xFF2C4F1C), Color(0xFFD7F5C4), Color(0xFF141A12), Color(0xFFE1E6DA),
        Color(0xFF1F281B), Color(0xFFA6B19C), Color(0xFF1F281B), Color(0xFFE6C06B), Color(0xFFFF8A80),
        Color(0xFF222D1D), Color(0xFF34442C), Color(0xFFB5E59A),
    ) else Palette(
        Color(0xFF3E7D2A), Color.White, Color(0xFFDDF0CF), Color(0xFF173807), Color(0xFFF7F8F1), Color(0xFF223019),
        Color(0xFFEBEEE2), Color(0xFF5B6852), Color(0xFFE3E8D8), Color(0xFF8D6E2B), Color(0xFFB3261E),
        Color(0xFFE7EBDD), Color(0xFFD2D9C4), Color(0xFF2E6A1E),
    )
    AppTheme.Dusk -> if (dark) Palette(
        Color(0xFFF2A3D0), Color(0xFF4A1238), Color(0xFF63254F), Color(0xFFFFD8EE), Color(0xFF1A1420), Color(0xFFEDE3F0),
        Color(0xFF261D2E), Color(0xFFB7A7C2), Color(0xFF261D2E), Color(0xFFB49CFF), Color(0xFFFF8A80),
        Color(0xFF2B2135), Color(0xFF3E3049), Color(0xFFFFB3DD),
    ) else Palette(
        Color(0xFFA63D7F), Color.White, Color(0xFFFADDEE), Color(0xFF3D0A2E), Color(0xFFFCF7FA), Color(0xFF2E1C2A),
        Color(0xFFF3EAF2), Color(0xFF6E5B6B), Color(0xFFEDE2EE), Color(0xFF6E56CF), Color(0xFFB3261E),
        Color(0xFFF1E6EF), Color(0xFFE0D0DE), Color(0xFF9C2F74),
    )
    AppTheme.Contrast -> if (dark) Palette(
        Color(0xFFFFE44D), Color.Black, Color(0xFF3A3300), Color(0xFFFFF7B0), Color.Black, Color.White,
        Color(0xFF121212), Color(0xFFD6D6D6), Color(0xFF1A1A1A), Color(0xFF00E676), Color(0xFFFF5252),
        Color(0xFF1C1C1C), Color(0xFF5A5A5A), Color(0xFF7CC4FF),
    ) else Palette(
        Color(0xFF0033CC), Color.White, Color(0xFFD6E0FF), Color.Black, Color.White, Color.Black,
        Color(0xFFF2F2F2), Color(0xFF333333), Color(0xFFE6E6E6), Color(0xFF00695C), Color(0xFFB00020),
        Color(0xFFEDEDED), Color(0xFF8A8A8A), Color(0xFF0033CC),
    )
}

private fun scheme(p: Palette, dark: Boolean): ColorScheme {
    val base = if (dark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = p.primary,
        onPrimary = p.onPrimary,
        primaryContainer = p.primaryContainer,
        onPrimaryContainer = p.onPrimaryContainer,
        secondary = p.muted,
        secondaryContainer = p.secondaryContainer,
        onSecondaryContainer = p.onBackground,
        tertiary = p.tertiary,
        background = p.background,
        onBackground = p.onBackground,
        surface = p.background,
        onSurface = p.onBackground,
        surfaceVariant = p.card,
        onSurfaceVariant = p.muted,
        surfaceContainerLowest = p.background,
        surfaceContainerLow = p.card,
        surfaceContainer = p.card,
        surfaceContainerHigh = p.card,
        surfaceContainerHighest = p.workBorder,
        outline = p.workBorder,
        outlineVariant = p.workBorder,
        error = p.error,
    )
}

val Mono = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 13.sp, lineHeight = 18.sp)

@Composable
fun PoTheme(theme: AppTheme = AppTheme.Po, appearance: Appearance = Appearance.System, content: @Composable () -> Unit) {
    val dark = when (appearance) {
        Appearance.System -> isSystemInDarkTheme()
        Appearance.Light -> false
        Appearance.Dark -> true
    }
    val p = palette(theme, dark)
    val extra = PoExtra(work = p.work, workBorder = p.workBorder, link = p.link, ok = p.tertiary, warn = PoColors.Amber)
    CompositionLocalProvider(LocalPo provides extra) {
        MaterialTheme(colorScheme = scheme(p, dark), typography = Typography(), content = content)
    }
}

/** The panda's head, from the same pixel grid as the terminal's mascot (left half, mirrored). */
private val HALF = arrayOf(
    "..KKK.....", ".KKKKK....", ".KKKKK....", "..KKKKKKKK", ".KWWWWWWWW",
    "KWWWWWWWWW", "KWWKKKKWWW", "KWKKKKKKWW", "KWKKKKKKWW", "KWWKKKKWKK",
    "KWPPPWWWWW", "KWPPPWWWWW", ".KWWWWWWWW", "..KKWWWWWW", "....KKKKKK",
)

@Composable
fun Mascot(size: Dp = 96.dp) {
    Canvas(Modifier.size(size)) {
        val cols = 19
        val px = this.size.width / cols
        val top = (this.size.height - px * HALF.size) / 2
        HALF.forEachIndexed { y, half ->
            val row = half + half.dropLast(1).reversed()
            row.forEachIndexed { x, c ->
                val color = when (c) {
                    'K' -> PoColors.Ink
                    'W' -> PoColors.Cream
                    'P' -> PoColors.Pink
                    else -> null
                }
                if (color != null) drawRect(color, Offset(x * px, top + y * px), Size(px + 0.5f, px + 0.5f))
            }
        }
        // The eyes' glints.
        drawRect(Color.White, Offset(5 * px, top + 7 * px), Size(px, px))
        drawRect(Color.White, Offset(14 * px, top + 7 * px), Size(px, px))
    }
}
