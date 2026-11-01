package dev.po.remote.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/**
 * The few icons the app needs beyond Material's core set, drawn from their path data: the large
 * extended icon set is not worth its megabytes for a dozen glyphs.
 */
object PoIcons {
    private fun icon(name: String, vararg paths: String): ImageVector {
        val builder = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        paths.forEach { builder.addPath(addPathNodes(it), fill = SolidColor(Color.Black)) }
        return builder.build()
    }

    /** A modern microphone: a rounded capsule on its stand. */
    val Mic = icon(
        "Mic",
        "M12,14.5c1.93,0 3.5,-1.57 3.5,-3.5V5.5C15.5,3.57 13.93,2 12,2S8.5,3.57 8.5,5.5V11c0,1.93 1.57,3.5 3.5,3.5z",
        "M18.25,11a0.75,0.75 0,0 0,-1.5 0c0,2.62 -2.13,4.75 -4.75,4.75S7.25,13.62 7.25,11a0.75,0.75 0,0 0,-1.5 0c0,3.19 2.4,5.83 5.5,6.2V20H9a0.75,0.75 0,0 0,0 1.5h6a0.75,0.75 0,0 0,0 -1.5h-2.25v-2.8c3.1,-0.37 5.5,-3.01 5.5,-6.2z",
    )
    val Stop = icon("Stop", "M8,6h8c1.1,0 2,0.9 2,2v8c0,1.1 -0.9,2 -2,2H8c-1.1,0 -2,-0.9 -2,-2V8c0,-1.1 0.9,-2 2,-2z")
    val Pause = icon("Pause", "M7,5h3a1,1 0,0 1,1 1v12a1,1 0,0 1,-1 1H7a1,1 0,0 1,-1 -1V6a1,1 0,0 1,1 -1zM14,5h3a1,1 0,0 1,1 1v12a1,1 0,0 1,-1 1h-3a1,1 0,0 1,-1 -1V6a1,1 0,0 1,1 -1z")
    val Play = icon("Play", "M8,5.14v13.72c0,0.79 0.87,1.27 1.54,0.84l10.29,-6.86c0.6,-0.4 0.6,-1.28 0,-1.68L9.54,4.3C8.87,3.87 8,4.35 8,5.14z")
    val Send = icon("Send", "M12,3.5c0.28,0 0.53,0.11 0.71,0.29l6,6a1,1 0,0 1,-1.42 1.42L13,6.91V19.5a1,1 0,1 1,-2 0V6.91l-4.29,4.3a1,1 0,0 1,-1.42 -1.42l6,-6c0.18,-0.18 0.43,-0.29 0.71,-0.29z")
    val Copy = icon(
        "Copy",
        "M16,1H4c-1.1,0 -2,0.9 -2,2v14h2V3h12V1z",
        "M19,5H8c-1.1,0 -2,0.9 -2,2v14c0,1.1 0.9,2 2,2h11c1.1,0 2,-0.9 2,-2V7c0,-1.1 -0.9,-2 -2,-2zM19,21H8V7h11v14z",
    )
    val Speak = icon(
        "Speak",
        "M3,9v6h4l5,5V4L7,9H3z",
        "M16.5,12c0,-1.77 -1.02,-3.29 -2.5,-4.03v8.05c1.48,-0.73 2.5,-2.25 2.5,-4.02z",
        "M14,3.23v2.06c2.89,0.86 5,3.54 5,6.71s-2.11,5.85 -5,6.71v2.06c4.01,-0.91 7,-4.49 7,-8.77s-2.99,-7.86 -7,-8.77z",
    )
    val Attach = icon(
        "Attach",
        "M16.5,6v11.5c0,2.21 -1.79,4 -4,4s-4,-1.79 -4,-4V5c0,-1.38 1.12,-2.5 2.5,-2.5s2.5,1.12 2.5,2.5v10.5c0,0.55 -0.45,1 -1,1s-1,-0.45 -1,-1V6H10v9.5c0,1.38 1.12,2.5 2.5,2.5s2.5,-1.12 2.5,-2.5V5c0,-2.21 -1.79,-4 -4,-4S7,2.79 7,5v12.5c0,3.04 2.46,5.5 5.5,5.5s5.5,-2.46 5.5,-5.5V6h-1.5z",
    )
    val Camera = icon(
        "Camera",
        "M9,2L7.17,4H4c-1.1,0 -2,0.9 -2,2v12c0,1.1 0.9,2 2,2h16c1.1,0 2,-0.9 2,-2V6c0,-1.1 -0.9,-2 -2,-2h-3.17L15,2H9zM12,17c-2.76,0 -5,-2.24 -5,-5s2.24,-5 5,-5 5,2.24 5,5 -2.24,5 -5,5z",
        "M12,15.2a3.2,3.2 0,1 0,0 -6.4a3.2,3.2 0,1 0,0 6.4z",
    )
    val Image = icon("Image", "M21,19V5c0,-1.1 -0.9,-2 -2,-2H5c-1.1,0 -2,0.9 -2,2v14c0,1.1 0.9,2 2,2h14c1.1,0 2,-0.9 2,-2zM8.5,13.5l2.5,3.01L14.5,12l4.5,6H5l3.5,-4.5z")
    val File = icon("File", "M14,2H6c-1.1,0 -1.99,0.9 -1.99,2L4,20c0,1.1 0.89,2 1.99,2H18c1.1,0 2,-0.9 2,-2V8l-6,-6zM16,18H8v-2h8v2zM16,14H8v-2h8v2zM13,9V3.5L18.5,9H13z")
    /** Subagents: a small tree of tasks. */
    val Agents = icon("Agents", "M22,11V3h-7v3H9V3H2v8h7V8h2v10h4v3h7v-8h-7v3h-2V8h2v3z")
    val Palette = icon(
        "Palette",
        "M12,3c-4.97,0 -9,4.03 -9,9s4.03,9 9,9c0.83,0 1.5,-0.67 1.5,-1.5 0,-0.39 -0.15,-0.74 -0.39,-1.01 -0.23,-0.26 -0.38,-0.61 -0.38,-0.99 0,-0.83 0.67,-1.5 1.5,-1.5H16c2.76,0 5,-2.24 5,-5 0,-4.42 -4.03,-8 -9,-8zM6.5,12c-0.83,0 -1.5,-0.67 -1.5,-1.5S5.67,9 6.5,9 8,9.67 8,10.5 7.33,12 6.5,12zM9.5,8C8.67,8 8,7.33 8,6.5S8.67,5 9.5,5s1.5,0.67 1.5,1.5S10.33,8 9.5,8zM14.5,8c-0.83,0 -1.5,-0.67 -1.5,-1.5S13.67,5 14.5,5s1.5,0.67 1.5,1.5S15.33,8 14.5,8zM17.5,12c-0.83,0 -1.5,-0.67 -1.5,-1.5S16.67,9 17.5,9s1.5,0.67 1.5,1.5 -0.67,1.5 -1.5,1.5z",
    )
    val Chevron = icon("Chevron", "M9.29,6.71a1,1 0,0 0,0 1.41L13.17,12l-3.88,3.88a1,1 0,1 0,1.41 1.41l4.59,-4.59a1,1 0,0 0,0 -1.41L10.7,6.7a1,1 0,0 0,-1.41 0.01z")
}
