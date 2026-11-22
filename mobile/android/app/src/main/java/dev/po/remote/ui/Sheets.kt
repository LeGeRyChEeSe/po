package dev.po.remote.ui

import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.material3.TextButton
import uniffi.po_mobile.ItemKind
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.foundation.BorderStroke

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.unit.Velocity
import androidx.compose.material3.SheetValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.material3.HorizontalDivider
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.po.remote.AppState
import dev.po.remote.R
import kotlinx.coroutines.delay
import uniffi.po_mobile.InputRec
import uniffi.po_mobile.ItemRec
import uniffi.po_mobile.ToolStatus

/** A bar from green to red, filled to `fraction`, as Claude Code's `/context` draws it. */
@Composable
fun UsageBar(fraction: Float, modifier: Modifier = Modifier, height: Int = 10) {
    val f = fraction.coerceIn(0f, 1f)
    val color = gaugeColor(f)
    Box(modifier.fillMaxWidth().height(height.dp).clip(CircleShape).background(color.copy(alpha = 0.18f))) {
        Box(Modifier.fillMaxWidth(f).fillMaxHeight().clip(CircleShape).background(color))
    }
}

@Composable
private fun InfoRow(label: String, value: String) {
    // Both sides wrap: a long model name goes over several lines instead of crushing its label.
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f).padding(end = 12.dp), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
        Text(value, Modifier.weight(1.4f), style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium, textAlign = TextAlign.End)
    }
}

@Composable
private fun SheetSection(title: String) {
    Text(title, Modifier.padding(top = 14.dp, bottom = 4.dp), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
}

internal fun tokens(n: Long): String = when {
    n >= 1_000_000 -> String.format("%.1fM", n / 1e6)
    n >= 1_000 -> String.format("%.1fk", n / 1e3)
    else -> n.toString()
}

/** A circular progress gauge with percentage in center and descriptive labels. */
@Composable
fun CircularProgressGauge(
    fraction: Float,
    label: String,
    sublabel: String = "",
    size: Int = 80,
    strokeWidth: Int = 7,
    color: Color? = null,
    modifier: Modifier = Modifier,
) {
    val f = fraction.coerceIn(0f, 1f)
    val ringColor = color ?: gaugeColor(f)
    Column(
        modifier = modifier,
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Box(
            modifier = Modifier.size(size.dp),
            contentAlignment = Alignment.Center
        ) {
            CircularProgressIndicator(
                progress = { f },
                modifier = Modifier.size(size.dp),
                color = ringColor,
                trackColor = ringColor.copy(alpha = 0.18f),
                strokeWidth = strokeWidth.dp,
            )
            Text(
                "${(f * 100).toInt()}%",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = ringColor,
            )
        }
        if (label.isNotBlank()) {
            Spacer(Modifier.height(6.dp))
            Text(
                label,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.SemiBold,
                textAlign = TextAlign.Center,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
        }
        if (sublabel.isNotBlank()) {
            Text(
                sublabel,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
        }
    }
}

@Composable
private fun BreakdownRow(dotColor: Color, label: String, tokens: Long, total: Long) {
    val pct = ((tokens.toFloat() / total.toFloat().coerceAtLeast(1f)) * 100).toInt()
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(10.dp).clip(CircleShape).background(dotColor))
        Spacer(Modifier.width(12.dp))
        Text(label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f).padding(end = 12.dp), maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text(
            "${tokens(tokens)} ($pct%)",
            maxLines = 1,
            softWrap = false,
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.onSurface
        )
    }
}

/**
 * Detailed Context Sheet: Circular fullness gauge, exact token breakdown
 * (tool calls, reasoning, assistant generation, user messages, system overhead),
 * speeds, and compaction action.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ContextSheet(state: AppState, onClose: () -> Unit) {
    val status = state.status ?: return onClose()
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onClose, sheetState = sheet) {
        Column(
            Modifier
                .navigationBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp)
        ) {
            Text(
                stringResource(R.string.context_title),
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold
            )

            val used = status.contextUsed.toLong()
            val window = status.contextWindow.toLong().coerceAtLeast(1)
            val fraction = (used.toFloat() / window).coerceIn(0f, 1f)
            val left = (window - used).coerceAtLeast(0)

            // Circular Context Gauge
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
                modifier = Modifier.fillMaxWidth()
            ) {
                Column(
                    Modifier.padding(16.dp),
                    horizontalAlignment = Alignment.CenterHorizontally
                ) {
                    CircularProgressGauge(
                        fraction = fraction,
                        label = stringResource(R.string.context_tokens, tokens(used), tokens(window)),
                        sublabel = stringResource(R.string.context_left, tokens(left)),
                        size = 96,
                        strokeWidth = 8,
                    )
                }
            }

            // Token Distribution / Breakdown
            val toolTokens: Long
            val thinkingTokens: Long
            val assistantTokens: Long
            val userTokens: Long
            val systemTokens: Long

            if (status.contextTools > 0u || status.contextSystem > 0u || status.contextUser > 0u || status.contextAssistant > 0u) {
                toolTokens = status.contextTools.toLong()
                thinkingTokens = status.contextThinking.toLong()
                assistantTokens = status.contextAssistant.toLong()
                userTokens = status.contextUser.toLong()
                systemTokens = status.contextSystem.toLong()
            } else {
                toolTokens = state.rows.filter { it.kind == ItemKind.TOOL }.sumOf {
                    val c = if (it.chars > 0u) it.chars else (it.title.length + it.text.length + it.detail.length + it.output.length).toULong()
                    (c / 4u).toLong()
                }
                thinkingTokens = state.rows.filter { it.kind == ItemKind.THINKING }.sumOf {
                    val c = if (it.chars > 0u) it.chars else it.text.length.toULong()
                    (c / 4u).toLong()
                }
                assistantTokens = state.rows.filter { it.kind == ItemKind.ASSISTANT }.sumOf {
                    val c = if (it.chars > 0u) it.chars else it.text.length.toULong()
                    (c / 4u).toLong()
                }
                userTokens = state.rows.filter { it.kind == ItemKind.USER }.sumOf {
                    val c = if (it.chars > 0u) it.chars else it.text.length.toULong()
                    (c / 4u).toLong()
                }
                val measured = toolTokens + thinkingTokens + assistantTokens + userTokens
                systemTokens = (used - measured).coerceAtLeast(0)
            }

            SheetSection(stringResource(R.string.context_breakdown_title))

            Surface(
                shape = RoundedCornerShape(14.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier.fillMaxWidth()
            ) {
                Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    val validUsed = used.coerceAtLeast(1)
                    val pTool = (toolTokens.toFloat() / validUsed).coerceIn(0f, 1f)
                    val pThink = (thinkingTokens.toFloat() / validUsed).coerceIn(0f, 1f)
                    val pAssist = (assistantTokens.toFloat() / validUsed).coerceIn(0f, 1f)
                    val pUser = (userTokens.toFloat() / validUsed).coerceIn(0f, 1f)
                    val pSys = (systemTokens.toFloat() / validUsed).coerceIn(0f, 1f)

                    Row(
                        Modifier
                            .fillMaxWidth()
                            .height(10.dp)
                            .clip(CircleShape)
                            .background(MaterialTheme.colorScheme.surface)
                    ) {
                        if (pTool > 0f) Box(Modifier.weight(pTool.coerceAtLeast(0.01f)).fillMaxHeight().background(Color(0xFF9D7CD8)))
                        if (pThink > 0f) Box(Modifier.weight(pThink.coerceAtLeast(0.01f)).fillMaxHeight().background(PoColors.Amber))
                        if (pAssist > 0f) Box(Modifier.weight(pAssist.coerceAtLeast(0.01f)).fillMaxHeight().background(PoColors.Moss))
                        if (pUser > 0f) Box(Modifier.weight(pUser.coerceAtLeast(0.01f)).fillMaxHeight().background(Color(0xFF4F8EF7)))
                        if (pSys > 0f) Box(Modifier.weight(pSys.coerceAtLeast(0.01f)).fillMaxHeight().background(Color.Gray.copy(alpha = 0.5f)))
                    }

                    BreakdownRow(Color(0xFF9D7CD8), stringResource(R.string.context_breakdown_tools), toolTokens, validUsed)
                    BreakdownRow(PoColors.Amber, stringResource(R.string.context_breakdown_reasoning), thinkingTokens, validUsed)
                    BreakdownRow(PoColors.Moss, stringResource(R.string.context_breakdown_assistant), assistantTokens, validUsed)
                    BreakdownRow(Color(0xFF4F8EF7), stringResource(R.string.context_breakdown_user), userTokens, validUsed)
                    BreakdownRow(Color.Gray, stringResource(R.string.context_breakdown_system), systemTokens, validUsed)
                }
            }

            // Speeds: one line each, the figure of now on the right, never cut nor wrapped.
            if (status.generationTokS > 0f || status.prefillTokS > 0f || status.instantTokS > 0f) {
                SheetSection(stringResource(R.string.context_speeds_title))
                Surface(shape = RoundedCornerShape(14.dp), color = MaterialTheme.colorScheme.surfaceVariant, modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(horizontal = 16.dp, vertical = 6.dp)) {
                        val writeNow = if (status.instantTokS > 0f) status.instantTokS else status.generationTokS
                        val readNow = if (status.lastPrefillTokS > 0f) status.lastPrefillTokS else status.prefillTokS
                        SpeedLine(
                            stringResource(R.string.context_generation_title),
                            stringResource(R.string.context_speed_average) + " · " + speed(status.generationTokS, 1),
                            speed(writeNow, 1),
                            MaterialTheme.colorScheme.primary,
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.4f))
                        SpeedLine(
                            stringResource(R.string.context_prefill_title),
                            stringResource(R.string.context_speed_average) + " · " + speed(status.prefillTokS, 0),
                            speed(readNow, 0),
                            PoColors.Moss,
                        )
                    }
                }
                Text(
                    stringResource(R.string.context_speed_note),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.75f),
                    lineHeight = 18.sp,
                )
            }

            Spacer(Modifier.height(14.dp))

            Button(
                onClick = { onClose(); state.send(InputRec.Command("/compacter")) },
                modifier = Modifier.fillMaxWidth().height(44.dp)
            ) {
                Text(stringResource(R.string.compact))
            }
        }
    }
}

/** "63.8 tok/s", or a dash when nothing was measured. */
private fun speed(value: Float, decimals: Int): String =
    if (value > 0f) String.format(java.util.Locale.getDefault(), "%.${decimals}f tok/s", value) else "–"

/** A speed: what it is and its average on the left, the figure of now on the right. */
@Composable
private fun SpeedLine(title: String, average: String, now: String, color: Color) {
    Row(Modifier.fillMaxWidth().padding(vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f).padding(end = 12.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Spacer(Modifier.height(2.dp))
            Text(average, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Text(now, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold, color = color, maxLines = 1, softWrap = false)
    }
}

private fun copy(context: android.content.Context, text: String) {
    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("Po", text))
}

/** Takes whatever scrolling is left over, so a code box at its end does not hand it to the sheet. */
private object KeepScroll : NestedScrollConnection {
    override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset = available
    override suspend fun onPostFling(consumed: Velocity, available: Velocity): Velocity = available
}

/** Tells [onHold] when a finger rests on this area and when it leaves. The touches are only watched
 *  (never consumed), so the scrolling and the selection inside keep working. */
private fun Modifier.holding(onHold: (Boolean) -> Unit): Modifier = pointerInput(Unit) {
    awaitEachGesture {
        awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
        onHold(true)
        do {
            val event = awaitPointerEvent(PointerEventPass.Initial)
        } while (event.changes.any { it.pressed })
        onHold(false)
    }
}

/** A box of text with its title and a copy button: a call, its output. */
@Composable
private fun CodeBox(title: String, text: String, onHold: (Boolean) -> Unit = {}) {
    val context = LocalContext.current
    Row(Modifier.fillMaxWidth().padding(top = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        IconButton(onClick = { copy(context, text) }, Modifier.size(32.dp)) { Icon(PoIcons.Copy, stringResource(R.string.copy), Modifier.size(18.dp)) }
    }
    Surface(Modifier.fillMaxWidth().heightIn(max = 360.dp).holding(onHold).nestedScroll(KeepScroll), shape = RoundedCornerShape(10.dp), color = LocalPo.current.work) {
        SelectionContainer {
            Text(
                text,
                Modifier.verticalScroll(rememberScrollState()).horizontalScroll(rememberScrollState()).padding(12.dp),
                style = Mono,
                softWrap = false,
            )
        }
    }
}

/** A tool call in full: what it was asked, what it gave back, the change it made. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ToolSheet(row: ItemRec, onClose: () -> Unit) {
    // A gesture begun on the code (to scroll it sideways, to select) never closes the sheet: a slight
    // vertical move while reading a long command used to. `holding` stays on a moment after the
    // finger leaves, so the sheet settling at the release still sees it.
    var holding by remember { mutableStateOf(false) }
    val held by rememberUpdatedState(holding)
    val scope = rememberCoroutineScope()
    var release by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    val hold: (Boolean) -> Unit = { down ->
        release?.cancel()
        if (down) holding = true
        else release = scope.launch { kotlinx.coroutines.delay(400); holding = false }
    }
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true, confirmValueChange = { it != SheetValue.Hidden || !held })
    ModalBottomSheet(onDismissRequest = onClose, sheetState = sheet) {
        Column(Modifier.navigationBarsPadding().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp).padding(bottom = 20.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(row.title, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                    Text(
                        stringResource(
                            when (row.status) {
                                ToolStatus.RUNNING -> R.string.tool_running
                                ToolStatus.ERROR -> R.string.tool_error
                                ToolStatus.DENIED -> R.string.tool_denied
                                else -> R.string.tool_done
                            }
                        ),
                        color = if (row.status == ToolStatus.ERROR || row.status == ToolStatus.DENIED) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                IconButton(onClick = onClose) { Icon(Icons.Default.Close, stringResource(R.string.close)) }
            }
            CodeBox(stringResource(R.string.tool_call), row.text, hold)
            when {
                row.output.isNotBlank() -> CodeBox(stringResource(R.string.tool_output), row.output, hold)
                row.status == ToolStatus.RUNNING -> Text(stringResource(R.string.tool_running), Modifier.padding(top = 12.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (row.detail.isNotBlank()) {
                Text(stringResource(R.string.tool_diff), Modifier.padding(top = 12.dp, bottom = 4.dp), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                DiffBox(row.detail, hold)
            }
        }
    }
}

@Composable
private fun DiffBox(diff: String, onHold: (Boolean) -> Unit = {}) {
    Surface(Modifier.holding(onHold), color = LocalPo.current.work, shape = RoundedCornerShape(10.dp)) {
        SelectionContainer {
            Column(Modifier.horizontalScroll(rememberScrollState()).padding(10.dp)) {
                diff.lines().take(600).forEach { line ->
                    val (fg, bg) = when {
                        line.startsWith("+") -> PoColors.Moss to PoColors.Moss.copy(alpha = 0.12f)
                        line.startsWith("-") -> PoColors.Red to PoColors.Red.copy(alpha = 0.12f)
                        line.startsWith("@@") -> MaterialTheme.colorScheme.primary to Color.Transparent
                        else -> MaterialTheme.colorScheme.onSurface to Color.Transparent
                    }
                    Text(line, Modifier.background(bg), style = Mono, color = fg, softWrap = false)
                }
            }
        }
    }
}

@Composable
private fun Tile(icon: ImageVector, label: String, modifier: Modifier, onClick: () -> Unit) {
    val shape = RoundedCornerShape(16.dp)
    Column(
        modifier.clip(shape).border(1.dp, MaterialTheme.colorScheme.outline, shape).clickable(onClick = onClick).padding(vertical = 18.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(icon, null, Modifier.size(26.dp))
        Spacer(Modifier.height(8.dp))
        Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** "+": the camera, the photos, the files - several at once. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AttachSheet(onCamera: () -> Unit, onPhotos: () -> Unit, onFiles: () -> Unit, onClose: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onClose) {
        Column(Modifier.navigationBarsPadding().padding(horizontal = 16.dp).padding(bottom = 20.dp)) {
            Text(stringResource(R.string.attach_title), Modifier.fillMaxWidth().padding(bottom = 14.dp), style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Tile(PoIcons.Camera, stringResource(R.string.attach_camera), Modifier.weight(1f)) { onClose(); onCamera() }
                Tile(PoIcons.Image, stringResource(R.string.attach_photo), Modifier.weight(1f)) { onClose(); onPhotos() }
                Tile(PoIcons.File, stringResource(R.string.attach_file), Modifier.weight(1f)) { onClose(); onFiles() }
            }
            Text(stringResource(R.string.attach_hint), Modifier.padding(top = 12.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/**
 * Work mode sheet: Plan, Builder, Bypass with modern, ergonomic, responsive cards.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WorkModeSheet(state: AppState, currentMode: String, onClose: () -> Unit) {
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onClose, sheetState = sheet) {
        Column(
            Modifier
                .navigationBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)
        ) {
            Text(
                stringResource(R.string.mode_sheet_title),
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.padding(bottom = 4.dp)
            )

            ModeCard(
                name = stringResource(R.string.mode_plan),
                tag = stringResource(R.string.mode_plan_tag),
                description = stringResource(R.string.mode_plan_desc),
                icon = PoIcons.Plan,
                accentColor = Color(0xFF4F8EF7),
                selected = currentMode == "plan",
                onClick = {
                    onClose()
                    state.send(InputRec.Command("/plan"))
                }
            )

            ModeCard(
                name = stringResource(R.string.mode_build),
                tag = stringResource(R.string.mode_build_tag),
                description = stringResource(R.string.mode_build_desc),
                icon = PoIcons.Builder,
                accentColor = LocalPo.current.ok,
                selected = currentMode == "builder",
                onClick = {
                    onClose()
                    state.send(InputRec.Command("/builder"))
                }
            )

            ModeCard(
                name = stringResource(R.string.mode_bypass),
                tag = stringResource(R.string.mode_bypass_tag),
                description = stringResource(R.string.mode_bypass_desc),
                icon = PoIcons.Bypass,
                accentColor = MaterialTheme.colorScheme.error,
                selected = currentMode == "bypass",
                onClick = {
                    onClose()
                    state.send(InputRec.Command("/bypass"))
                }
            )

            // Auto last, its cost on the card: measured no more accurate than Builder, ~3x slower.
            // It is for a long unattended task, not an everyday "better" mode.
            ModeCard(
                name = stringResource(R.string.mode_auto),
                tag = stringResource(R.string.mode_auto_tag),
                description = stringResource(R.string.mode_auto_desc),
                icon = PoIcons.Auto,
                accentColor = Color(0xFF9D7CD8),
                selected = currentMode == "auto",
                onClick = {
                    onClose()
                    state.send(InputRec.Command("/auto"))
                }
            )

            ModeCard(
                name = stringResource(R.string.mode_auto_bypass),
                tag = stringResource(R.string.mode_bypass_tag),
                description = stringResource(R.string.mode_auto_bypass_desc),
                icon = PoIcons.Auto,
                accentColor = MaterialTheme.colorScheme.error,
                selected = currentMode == "auto · bypass",
                onClick = {
                    onClose()
                    state.send(InputRec.Command("/auto-bypass"))
                }
            )
        }
    }
}

@Composable
private fun ModeCard(
    name: String,
    tag: String,
    description: String,
    icon: ImageVector,
    accentColor: Color,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val shape = RoundedCornerShape(16.dp)
    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .clickable(onClick = onClick)
            .border(
                width = if (selected) 2.dp else 1.dp,
                color = if (selected) accentColor else MaterialTheme.colorScheme.outlineVariant,
                shape = shape
            ),
        shape = shape,
        color = if (selected) accentColor.copy(alpha = 0.10f) else MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
    ) {
        Row(
            Modifier.padding(16.dp),
            verticalAlignment = Alignment.Top
        ) {
            Box(
                Modifier
                    .size(42.dp)
                    .clip(CircleShape)
                    .background(accentColor.copy(alpha = 0.2f)),
                contentAlignment = Alignment.Center
            ) {
                Icon(icon, null, Modifier.size(22.dp), tint = accentColor)
            }
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                    Spacer(Modifier.width(8.dp))
                    Surface(
                        shape = CircleShape,
                        color = accentColor.copy(alpha = 0.2f),
                    ) {
                        Text(
                            tag,
                            Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                            style = MaterialTheme.typography.labelSmall,
                            color = accentColor,
                            fontWeight = FontWeight.SemiBold
                        )
                    }
                }
                Spacer(Modifier.height(4.dp))
                Text(
                    description,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )
            }
            if (selected) {
                Spacer(Modifier.width(8.dp))
                Icon(Icons.Default.Check, stringResource(R.string.mode_active), tint = accentColor, modifier = Modifier.size(22.dp))
            }
        }
    }
}

/**
 * Modern 1-click Hardware & Model Sheet:
 * - Active Model info & 1-click Model Unload (Free GPU VRAM)
 * - Compartmentalized Categories with circular progress gauges:
 *   1) GPUs (VRAM % & Core Load %, Temp)
 *   2) Host System (CPU % & RAM %)
 *   3) Disks (Storage)
 *   4) Detected Local Models (1-click launch & set default)
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HardwareSheet(state: AppState, pcId: String?, onClose: () -> Unit) {
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    LaunchedEffect(pcId) {
        state.refreshUsage(pcId)
        while (true) {
            delay(2000)
            state.refreshUsage(pcId)
        }
    }
    val status = state.status
    val usage = state.usage

    ModalBottomSheet(onDismissRequest = onClose, sheetState = sheet) {
        Column(
            Modifier
                .navigationBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp)
        ) {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically
            ) {
                Icon(PoIcons.Hardware, null, Modifier.size(24.dp), tint = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.width(10.dp))
                Text(
                    stringResource(R.string.hardware_title),
                    style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.Bold,
                    modifier = Modifier.weight(1f)
                )
                IconButton(onClick = onClose) {
                    Icon(Icons.Default.Close, stringResource(R.string.close))
                }
            }

            // 1. Active Model & Unload Button
            val activeModelName = status?.pinned ?: status?.model?.ifBlank { null }
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                modifier = Modifier.fillMaxWidth()
            ) {
                Column(Modifier.padding(16.dp)) {
                    // The label, then the cards it runs on below it: side by side, a long placement
                    // squeezed the label into one letter per line.
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(
                            stringResource(R.string.hardware_model),
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.primary,
                        )
                        if (status?.placement != null) {
                            Surface(
                                shape = CircleShape,
                                color = PoColors.Moss.copy(alpha = 0.15f),
                                border = BorderStroke(1.dp, PoColors.Moss.copy(alpha = 0.4f))
                            ) {
                                Row(
                                    Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                                    verticalAlignment = Alignment.CenterVertically
                                ) {
                                    Box(Modifier.size(6.dp).clip(CircleShape).background(PoColors.Moss))
                                    Spacer(Modifier.width(6.dp))
                                    Text(
                                        status.placement!!,
                                        style = MaterialTheme.typography.labelSmall,
                                        fontWeight = FontWeight.SemiBold,
                                        color = PoColors.Moss
                                    )
                                }
                            }
                        }
                    }
                    Spacer(Modifier.height(4.dp))
                    Text(
                        activeModelName ?: "-",
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.Bold
                    )
                    if (status?.engine?.isNotBlank() == true || status?.effort != null) {
                        Spacer(Modifier.height(4.dp))
                        val details = listOfNotNull(
                            stringResource(R.string.model_not_in_memory).takeIf { status.modelLoaded == false },
                            status.engine.takeIf { it.isNotBlank() },
                            status.effort?.let { "${stringResource(R.string.context_thinking)}: $it" }
                        ).joinToString(" · ")
                        Text(
                            details,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }

                    if (activeModelName != null && activeModelName != "-" && status?.modelLoaded != false) {
                        Spacer(Modifier.height(12.dp))
                        OutlinedButton(
                            onClick = {
                                state.unloadModel(pcId)
                                state.refreshUsage(pcId)
                            },
                            modifier = Modifier.fillMaxWidth(),
                            colors = androidx.compose.material3.ButtonDefaults.outlinedButtonColors(
                                contentColor = MaterialTheme.colorScheme.error
                            ),
                            border = BorderStroke(1.dp, MaterialTheme.colorScheme.error.copy(alpha = 0.5f))
                        ) {
                            Icon(PoIcons.Power, null, Modifier.size(16.dp))
                            Spacer(Modifier.width(8.dp))
                            Text(stringResource(R.string.model_unload), fontWeight = FontWeight.SemiBold)
                        }
                    }
                }
            }

            // 2. GPUs Category
            Text(
                stringResource(R.string.hardware_category_gpu),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = PoColors.Moss
            )

            if (usage == null) {
                Row(
                    Modifier.padding(vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(10.dp))
                    Text(stringResource(R.string.loading_usage), color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            } else if (usage.gpus.isEmpty()) {
                Surface(
                    shape = RoundedCornerShape(12.dp),
                    color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text(
                        stringResource(R.string.no_gpu),
                        Modifier.padding(14.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }
            } else {
                usage.gpus.forEachIndexed { idx, gpu ->
                    Surface(
                        shape = RoundedCornerShape(16.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        border = BorderStroke(1.dp, PoColors.Moss.copy(alpha = 0.25f)),
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Column(Modifier.padding(16.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    "GPU $idx · ${gpu.name}",
                                    style = MaterialTheme.typography.titleMedium,
                                    fontWeight = FontWeight.Bold,
                                    modifier = Modifier.weight(1f)
                                )
                                gpu.temperatureC?.let {
                                    Surface(
                                        shape = CircleShape,
                                        color = if (it > 80u) PoColors.Red.copy(alpha = 0.15f) else MaterialTheme.colorScheme.surface
                                    ) {
                                        Text(
                                            "$it °C",
                                            Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                                            style = MaterialTheme.typography.labelSmall,
                                            fontWeight = FontWeight.Bold,
                                            color = if (it > 80u) PoColors.Red else MaterialTheme.colorScheme.onSurfaceVariant
                                        )
                                    }
                                }
                            }
                            Spacer(Modifier.height(14.dp))
                            val vramTotal = gpu.vramTotalMib.toLong().coerceAtLeast(1)
                            val vramUsed = gpu.vramUsedMib.toLong()
                            val fracVram = vramUsed.toFloat() / vramTotal
                            val fracLoad = gpu.percent / 100f

                            Row(
                                Modifier.fillMaxWidth(),
                                horizontalArrangement = Arrangement.SpaceEvenly
                            ) {
                                CircularProgressGauge(
                                    fraction = fracVram,
                                    label = stringResource(R.string.context_vram),
                                    sublabel = stringResource(R.string.context_gb_of, vramUsed / 1024.0, vramTotal / 1024.0),
                                    size = 78,
                                    strokeWidth = 6,
                                )
                                CircularProgressGauge(
                                    fraction = fracLoad,
                                    label = stringResource(R.string.gpu_load),
                                    sublabel = "${gpu.percent.toInt()} %",
                                    size = 78,
                                    strokeWidth = 6,
                                    color = PoColors.Moss
                                )
                            }
                        }
                    }
                }
            }

            // 3. Host System Category (CPU & RAM)
            if (usage != null) {
                Text(
                    stringResource(R.string.hardware_category_system),
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.primary
                )
                Surface(
                    shape = RoundedCornerShape(16.dp),
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Column(Modifier.padding(16.dp)) {
                        val ramTotal = usage.ramTotalMib.toLong().coerceAtLeast(1)
                        val ramUsed = usage.ramUsedMib.toLong()
                        val fracRam = ramUsed.toFloat() / ramTotal
                        val fracCpu = usage.cpuPercent / 100f

                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.SpaceEvenly
                        ) {
                            CircularProgressGauge(
                                fraction = fracCpu,
                                label = stringResource(R.string.context_cpu),
                                sublabel = "${usage.cpuPercent.toInt()} %",
                                size = 78,
                                strokeWidth = 6,
                                color = MaterialTheme.colorScheme.primary
                            )
                            CircularProgressGauge(
                                fraction = fracRam,
                                label = stringResource(R.string.context_ram),
                                sublabel = stringResource(R.string.context_gb_of, ramUsed / 1024.0, ramTotal / 1024.0),
                                size = 78,
                                strokeWidth = 6,
                                color = Color(0xFF4F8EF7)
                            )
                        }
                    }
                }
            }

            // 4. Disks Category
            if (usage != null && usage.disks.isNotEmpty()) {
                Text(
                    stringResource(R.string.hardware_category_disks),
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onSurface
                )
                usage.disks.forEach { disk ->
                    Surface(
                        shape = RoundedCornerShape(12.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Column(Modifier.padding(12.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(disk.name, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                                Text(stringResource(R.string.context_disk_usage, disk.freeGib, disk.totalGib), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                            Spacer(Modifier.height(6.dp))
                            UsageBar(1f - disk.freeGib / disk.totalGib.coerceAtLeast(1f), height = 6)
                        }
                    }
                }
            }

            // 5. Detected Local Models
            val detectedModels = status?.models ?: emptyList()
            if (detectedModels.isNotEmpty()) {
                Text(
                    stringResource(R.string.models_on_pc),
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.primary
                )
                detectedModels.forEach { m ->
                    val isCurrent = m == activeModelName
                    Surface(
                        shape = RoundedCornerShape(14.dp),
                        color = if (isCurrent) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceVariant,
                        border = if (isCurrent) BorderStroke(1.dp, MaterialTheme.colorScheme.primary) else null,
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Row(
                            Modifier.padding(14.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(m, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
                                if (isCurrent) {
                                    Text(stringResource(R.string.default_active), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.primary)
                                }
                            }
                            if (!isCurrent) {
                                TextButton(
                                    onClick = {
                                        state.send(InputRec.Command("/model $m"))
                                        state.toast = "Lancement de $m..."
                                    }
                                ) {
                                    Icon(Icons.Default.PlayArrow, null, Modifier.size(16.dp))
                                    Spacer(Modifier.width(4.dp))
                                    Text(stringResource(R.string.model_launch_default))
                                }
                            } else {
                                Icon(Icons.Default.Check, null, tint = MaterialTheme.colorScheme.primary)
                            }
                        }
                    }
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(10.dp), modifier = Modifier.padding(top = 6.dp)) {
                Button(
                    onClick = {
                        onClose()
                        state.command("/model settings")
                    },
                    modifier = Modifier.weight(1f)
                ) {
                    Text(stringResource(R.string.hardware_model_settings))
                }
                OutlinedButton(
                    onClick = {
                        state.refreshUsage(pcId)
                    },
                    modifier = Modifier.weight(1f)
                ) {
                    Text(stringResource(R.string.refresh))
                }
            }
        }
    }
}

/**
 * 1-click Model selector with:
 * - 1-Click Reasoning Effort adjustment (off/low/medium/high)
 * - Active Model card with 1-click Unload Model (Free VRAM)
 * - Available models list
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ModelSheet(state: AppState, pinned: String?, models: List<String>, onClose: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onClose) {
        Column(
            Modifier
                .navigationBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)
        ) {
            Text(
                stringResource(R.string.model_title),
                Modifier.fillMaxWidth().padding(bottom = 2.dp),
                style = MaterialTheme.typography.titleLarge,
                textAlign = TextAlign.Center,
                fontWeight = FontWeight.Bold
            )

            // Current loading status & 1-click cancel
            val currentLoading = state.status?.loading ?: state.globalLoading
            if (currentLoading != null) {
                Surface(
                    shape = RoundedCornerShape(14.dp),
                    color = PoColors.Amber.copy(alpha = 0.12f),
                    border = BorderStroke(1.dp, PoColors.Amber.copy(alpha = 0.5f)),
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Row(
                        Modifier.padding(14.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = PoColors.Amber)
                        Spacer(Modifier.width(10.dp))
                        Column(Modifier.weight(1f)) {
                            Text(
                                stringResource(R.string.loading_model),
                                style = MaterialTheme.typography.labelSmall,
                                color = PoColors.Amber,
                                fontWeight = FontWeight.Bold
                            )
                            val desc = listOfNotNull(
                                currentLoading.model.takeIf { it.isNotBlank() },
                                currentLoading.stage.takeIf { it.isNotBlank() }
                            ).joinToString(" · ")
                            Text(
                                desc.ifBlank { stringResource(R.string.loading_starting) },
                                style = MaterialTheme.typography.bodyMedium,
                                fontWeight = FontWeight.SemiBold
                            )
                        }
                        Spacer(Modifier.width(8.dp))
                        OutlinedButton(
                            onClick = {
                                state.unloadModel()
                            },
                            colors = androidx.compose.material3.ButtonDefaults.outlinedButtonColors(
                                contentColor = MaterialTheme.colorScheme.error
                            ),
                            border = BorderStroke(1.dp, MaterialTheme.colorScheme.error.copy(alpha = 0.5f)),
                            contentPadding = PaddingValues(horizontal = 10.dp, vertical = 6.dp)
                        ) {
                            Icon(Icons.Default.Close, null, Modifier.size(14.dp))
                            Spacer(Modifier.width(4.dp))
                            Text(stringResource(R.string.cancel), style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
            }

            // 1-Click Reasoning Effort Selector
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier.fillMaxWidth()
            ) {
                Column(Modifier.padding(14.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(PoIcons.Brain, null, Modifier.size(18.dp), tint = MaterialTheme.colorScheme.primary)
                        Spacer(Modifier.width(8.dp))
                        Text(
                            stringResource(R.string.effort_title),
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.Bold,
                            color = MaterialTheme.colorScheme.primary
                        )
                    }
                    Spacer(Modifier.height(4.dp))
                    Text(
                        stringResource(R.string.effort_hint),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                    Spacer(Modifier.height(10.dp))
                    val currentEffort = state.status?.effort?.lowercase() ?: "medium"
                    val efforts = listOf(
                        "off" to R.string.effort_off,
                        "low" to R.string.effort_low,
                        "medium" to R.string.effort_medium,
                        "high" to R.string.effort_high
                    )
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(6.dp)
                    ) {
                        efforts.forEach { (key, res) ->
                            val selected = currentEffort == key || (key != "off" && currentEffort.contains(key))
                            val accent = when (key) {
                                "off" -> Color.Gray
                                "low" -> PoColors.Moss
                                "medium" -> MaterialTheme.colorScheme.primary
                                else -> PoColors.Amber
                            }
                            Surface(
                                modifier = Modifier
                                    .weight(1f)
                                    .clip(RoundedCornerShape(10.dp))
                                    .clickable {
                                        state.send(InputRec.Command("/effort $key"))
                                    }
                                    .border(
                                        width = if (selected) 2.dp else 1.dp,
                                        color = if (selected) accent else MaterialTheme.colorScheme.outlineVariant,
                                        shape = RoundedCornerShape(10.dp)
                                    ),
                                shape = RoundedCornerShape(10.dp),
                                color = if (selected) accent.copy(alpha = 0.18f) else MaterialTheme.colorScheme.surface,
                            ) {
                                Box(
                                    Modifier.padding(vertical = 10.dp),
                                    contentAlignment = Alignment.Center
                                ) {
                                    Text(
                                        stringResource(res),
                                        style = MaterialTheme.typography.labelMedium,
                                        fontWeight = if (selected) FontWeight.Bold else FontWeight.Normal,
                                        color = if (selected) accent else MaterialTheme.colorScheme.onSurface
                                    )
                                }
                            }
                        }
                    }
                }
            }

            // Active model & 1-click Unload
            val activeModel = pinned ?: state.status?.model?.ifBlank { null }
            if (activeModel != null && activeModel.isNotBlank()) {
                Surface(
                    shape = RoundedCornerShape(14.dp),
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Row(
                        Modifier.padding(14.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                stringResource(R.string.hardware_model),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                            Text(
                                activeModel,
                                style = MaterialTheme.typography.titleMedium,
                                fontWeight = FontWeight.Bold
                            )
                            if (state.status?.modelLoaded == false) {
                                Text(
                                    stringResource(R.string.model_not_in_memory),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant
                                )
                            }
                            if (state.status?.placement != null) {
                                Text(
                                    state.status!!.placement!!,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = PoColors.Moss
                                )
                            }
                            val vramMib = state.status?.modelVramMib
                            val ramMib = state.status?.modelRamMib
                            if (vramMib != null || ramMib != null) {
                                val parts = mutableListOf<String>()
                                if (vramMib != null && vramMib > 0u) {
                                    val gpuName = state.status?.modelGpuName?.let { " ($it)" } ?: ""
                                    parts.add(stringResource(R.string.vram_amount, vramMib.toFloat() / 1024f, gpuName))
                                }
                                if (ramMib != null && ramMib > 0u) {
                                    parts.add(stringResource(R.string.ram_amount, ramMib.toFloat() / 1024f))
                                }
                                if (parts.isNotEmpty()) {
                                    Spacer(Modifier.height(2.dp))
                                    Text(
                                        stringResource(R.string.model_memory_footprint, parts.joinToString(" · ")),
                                        style = MaterialTheme.typography.labelSmall,
                                        fontWeight = FontWeight.SemiBold,
                                        color = PoColors.Moss
                                    )
                                }
                            }
                        }
                        if (state.status?.modelLoaded != false) OutlinedButton(
                            onClick = {
                                onClose()
                                state.unloadModel()
                            },
                            colors = androidx.compose.material3.ButtonDefaults.outlinedButtonColors(
                                contentColor = MaterialTheme.colorScheme.error
                            ),
                            border = BorderStroke(1.dp, MaterialTheme.colorScheme.error.copy(alpha = 0.5f))
                        ) {
                            Icon(PoIcons.Power, null, Modifier.size(16.dp))
                            Spacer(Modifier.width(6.dp))
                            Text(stringResource(R.string.model_unload), style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
            }

            // Secondary Model Selection (if hardware resources allow a second model without overload)
            val secondaryCandidates = state.status?.secondaryCandidates ?: emptyList()
            if (secondaryCandidates.isNotEmpty()) {
                Surface(
                    shape = RoundedCornerShape(16.dp),
                    color = PoColors.Moss.copy(alpha = 0.08f),
                    border = BorderStroke(1.dp, PoColors.Moss.copy(alpha = 0.35f)),
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Column(Modifier.padding(14.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Icon(PoIcons.Hardware, null, Modifier.size(18.dp), tint = PoColors.Moss)
                            Spacer(Modifier.width(8.dp))
                            Column(Modifier.weight(1f)) {
                                Text(
                                    stringResource(R.string.model_secondary_title),
                                    style = MaterialTheme.typography.titleSmall,
                                    fontWeight = FontWeight.Bold,
                                    color = PoColors.Moss
                                )
                                Text(
                                    stringResource(R.string.model_secondary_subtitle),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant
                                )
                            }
                        }
                        Spacer(Modifier.height(10.dp))
                        secondaryCandidates.forEach { candidate ->
                            var chosenContext by rememberSaveable(candidate.name) { mutableStateOf(candidate.recommendedContext) }
                            Surface(
                                shape = RoundedCornerShape(12.dp),
                                color = MaterialTheme.colorScheme.surface,
                                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f)),
                                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
                            ) {
                                Column(Modifier.padding(12.dp)) {
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Text(candidate.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
                                        Surface(
                                            shape = RoundedCornerShape(6.dp),
                                            color = PoColors.Moss.copy(alpha = 0.15f)
                                        ) {
                                            Text(
                                                candidate.targetGpu,
                                                Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
                                                style = MaterialTheme.typography.labelSmall,
                                                color = PoColors.Moss,
                                                fontWeight = FontWeight.Bold
                                            )
                                        }
                                    }
                                    Spacer(Modifier.height(4.dp))
                                    Text(
                                        stringResource(R.string.model_secondary_recommended_context, candidate.recommendedContext) + " · " + stringResource(R.string.model_secondary_context, candidate.maxSafeContext),
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant
                                    )
                                    Spacer(Modifier.height(8.dp))
                                    Button(
                                        onClick = {
                                            onClose()
                                            state.send(InputRec.Command("/local start ${candidate.name}"))
                                            state.toast = "Lancement de ${candidate.name} sur ${candidate.targetGpu}…"
                                        },
                                        modifier = Modifier.fillMaxWidth(),
                                        colors = androidx.compose.material3.ButtonDefaults.buttonColors(
                                            containerColor = PoColors.Moss
                                        )
                                    ) {
                                        Text(stringResource(R.string.model_secondary_launch))
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Subagent Model & Concurrency Configuration
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier.fillMaxWidth()
            ) {
                Column(Modifier.padding(14.dp)) {
                    Text(
                        stringResource(R.string.model_subagents_title),
                        style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.Bold,
                        color = MaterialTheme.colorScheme.primary
                    )
                    Spacer(Modifier.height(6.dp))
                    val currentParallel = state.status?.subagentParallel ?: 1u
                    val isParallel = currentParallel > 1u
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        Surface(
                            modifier = Modifier.weight(1f).clip(RoundedCornerShape(10.dp)).clickable {
                                state.send(InputRec.Command("/agents parallel 1"))
                            }.border(
                                width = if (!isParallel) 2.dp else 1.dp,
                                color = if (!isParallel) PoColors.Moss else MaterialTheme.colorScheme.outlineVariant,
                                shape = RoundedCornerShape(10.dp)
                            ),
                            shape = RoundedCornerShape(10.dp),
                            color = if (!isParallel) PoColors.Moss.copy(alpha = 0.15f) else MaterialTheme.colorScheme.surface
                        ) {
                            Box(Modifier.padding(vertical = 8.dp, horizontal = 6.dp), contentAlignment = Alignment.Center) {
                                Text(
                                    stringResource(R.string.model_subagents_parallel_sequential),
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = if (!isParallel) FontWeight.Bold else FontWeight.Normal,
                                    textAlign = TextAlign.Center
                                )
                            }
                        }
                        Surface(
                            modifier = Modifier.weight(1f).clip(RoundedCornerShape(10.dp)).clickable {
                                state.send(InputRec.Command("/agents parallel 2"))
                            }.border(
                                width = if (isParallel) 2.dp else 1.dp,
                                color = if (isParallel) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant,
                                shape = RoundedCornerShape(10.dp)
                            ),
                            shape = RoundedCornerShape(10.dp),
                            color = if (isParallel) MaterialTheme.colorScheme.primary.copy(alpha = 0.15f) else MaterialTheme.colorScheme.surface
                        ) {
                            Box(Modifier.padding(vertical = 8.dp, horizontal = 6.dp), contentAlignment = Alignment.Center) {
                                Text(
                                    stringResource(R.string.model_subagents_parallel_concurrent),
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = if (isParallel) FontWeight.Bold else FontWeight.Normal,
                                    textAlign = TextAlign.Center
                                )
                            }
                        }
                    }
                }
            }

            Text(
                stringResource(R.string.models_available),
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(top = 4.dp)
            )

            ModelCard(stringResource(R.string.model_auto), stringResource(R.string.model_auto_hint), pinned == null) {
                onClose()
                state.send(InputRec.Command("/model auto"))
            }

            models.forEach { m ->
                ModelCard(m, "", m == pinned) {
                    onClose()
                    state.send(InputRec.Command("/model $m"))
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(10.dp), modifier = Modifier.padding(top = 6.dp)) {
                OutlinedButton(onClick = { onClose(); state.command("/model settings") }, Modifier.weight(1f)) { Text(stringResource(R.string.model_settings)) }
                OutlinedButton(onClick = { onClose(); state.command("/configurer") }, Modifier.weight(1f)) { Text(stringResource(R.string.local_models)) }
            }
        }
    }
}

@Composable
private fun ModelCard(name: String, hint: String, chosen: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(14.dp)
    Surface(Modifier.fillMaxWidth().clip(shape).clickable(onClick = onClick), shape = shape, color = MaterialTheme.colorScheme.surfaceVariant) {
        Row(Modifier.padding(horizontal = 16.dp, vertical = 14.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(name, style = MaterialTheme.typography.titleMedium, fontWeight = if (chosen) FontWeight.Bold else FontWeight.Medium)
                if (hint.isNotBlank()) Text(hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (chosen) Icon(Icons.Default.Check, null, tint = MaterialTheme.colorScheme.primary)
        }
    }
}
