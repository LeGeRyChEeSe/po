package dev.po.remote.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

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

private val Light = lightColorScheme(
    primary = PoColors.Red,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFFFE1E7),
    onPrimaryContainer = PoColors.Ink,
    secondary = PoColors.Ink,
    onSecondary = PoColors.Cream,
    secondaryContainer = Color(0xFFE8E9EE),
    onSecondaryContainer = PoColors.Ink,
    tertiary = PoColors.Moss,
    background = PoColors.Cream,
    onBackground = PoColors.Ink,
    surface = PoColors.Cream,
    onSurface = PoColors.Ink,
    surfaceVariant = Color(0xFFF1EFE8),
    onSurfaceVariant = Color(0xFF5D6274),
    error = Color(0xFFC0392B),
)

private val Dark = darkColorScheme(
    primary = PoColors.Pink,
    onPrimary = PoColors.Ink,
    primaryContainer = Color(0xFF5A2E39),
    onPrimaryContainer = Color(0xFFFFE1E7),
    secondary = Color(0xFFC9CCD8),
    onSecondary = PoColors.Night,
    secondaryContainer = PoColors.NightCard,
    onSecondaryContainer = Color(0xFFE6E7EC),
    tertiary = Color(0xFF8CCBA6),
    background = PoColors.Night,
    onBackground = Color(0xFFE9E8E3),
    surface = PoColors.Night,
    onSurface = Color(0xFFE9E8E3),
    surfaceVariant = PoColors.NightCard,
    onSurfaceVariant = Color(0xFFA9ADBB),
    error = Color(0xFFFF8A80),
)

val Mono = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 12.sp, lineHeight = 16.sp)

@Composable
fun PoTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) Dark else Light, typography = Typography(), content = content)
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
