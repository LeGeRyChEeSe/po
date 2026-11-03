package dev.po.remote.ui

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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
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

private fun tokens(n: Long): String = when {
    n >= 1_000_000 -> String.format("%.1fM", n / 1e6)
    n >= 1_000 -> String.format("%.1fk", n / 1e3)
    else -> n.toString()
}

/**
 * The context in detail: how full the window is, the model and its speeds, what the PC uses
 * right now (asked every two seconds while open), and the compaction at hand.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ContextSheet(state: AppState, onClose: () -> Unit) {
    val status = state.status ?: return onClose()
    LaunchedEffect(Unit) {
        while (true) {
            state.refreshUsage()
            delay(2000)
        }
    }
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onClose, sheetState = sheet) {
        Column(Modifier.navigationBarsPadding().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp).padding(bottom = 20.dp)) {
            Text(stringResource(R.string.context_title), style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(12.dp))
            val used = status.contextUsed.toLong()
            val window = status.contextWindow.toLong().coerceAtLeast(1)
            val fraction = used.toFloat() / window
            Row(verticalAlignment = Alignment.Bottom) {
                Text("${(fraction * 100).toInt()} %", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Bold, color = gaugeColor(fraction))
                Spacer(Modifier.width(10.dp))
                Text(stringResource(R.string.context_tokens, tokens(used), tokens(window)), Modifier.padding(bottom = 4.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Spacer(Modifier.height(8.dp))
            UsageBar(fraction, height = 14)
            Spacer(Modifier.height(4.dp))
            Text(stringResource(R.string.context_left, tokens((window - used).coerceAtLeast(0))), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)

            SheetSection(stringResource(R.string.context_model))
            InfoRow(stringResource(R.string.context_name), status.pinned ?: status.model.ifBlank { "—" })
            if (status.engine.isNotBlank()) InfoRow(stringResource(R.string.context_engine), status.engine)
            InfoRow(stringResource(R.string.context_thinking), status.effort ?: stringResource(R.string.context_auto))
            InfoRow(stringResource(R.string.context_generation), if (status.generationTokS > 0f) String.format("%.1f tok/s", status.generationTokS) else "—")
            InfoRow(stringResource(R.string.context_prefill), if (status.prefillTokS > 0f) String.format("%.0f tok/s", status.prefillTokS) else "—")

            SheetSection(stringResource(R.string.context_pc))
            val usage = state.usage
            if (usage == null) {
                Text(stringResource(R.string.loading_usage), color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                InfoRow(stringResource(R.string.context_cpu), "${usage.cpuPercent.toInt()} %")
                UsageBar(usage.cpuPercent / 100f, height = 6)
                Spacer(Modifier.height(6.dp))
                val ramTotal = usage.ramTotalMib.toLong().coerceAtLeast(1)
                InfoRow(stringResource(R.string.context_ram), stringResource(R.string.context_gb_of, usage.ramUsedMib.toLong() / 1024.0, ramTotal / 1024.0))
                UsageBar(usage.ramUsedMib.toLong().toFloat() / ramTotal, height = 6)
                usage.gpus.forEach { gpu ->
                    Spacer(Modifier.height(8.dp))
                    val vram = gpu.vramTotalMib.toLong().coerceAtLeast(1)
                    val temp = gpu.temperatureC?.let { " · ${it} °C" } ?: ""
                    InfoRow(gpu.name, "${gpu.percent.toInt()} %$temp")
                    InfoRow(stringResource(R.string.context_vram), stringResource(R.string.context_gb_of, gpu.vramUsedMib.toLong() / 1024.0, vram / 1024.0))
                    UsageBar(gpu.vramUsedMib.toLong().toFloat() / vram, height = 6)
                }
                if (usage.disks.isNotEmpty()) {
                    Spacer(Modifier.height(8.dp))
                    usage.disks.forEach { disk ->
                        InfoRow(stringResource(R.string.context_disk, disk.name), stringResource(R.string.context_disk_usage, disk.freeGib, disk.totalGib))
                        UsageBar(1f - disk.freeGib / disk.totalGib.coerceAtLeast(1f), height = 4)
                        Spacer(Modifier.height(4.dp))
                    }
                }
            }
            Spacer(Modifier.height(16.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Button(onClick = { onClose(); state.send(InputRec.Command("/compacter")) }, Modifier.weight(1f)) { Text(stringResource(R.string.compact)) }
                OutlinedButton(onClick = { onClose(); state.command("/profil") }, Modifier.weight(1f)) { Text(stringResource(R.string.model_settings)) }
            }
        }
    }
}

private fun copy(context: android.content.Context, text: String) {
    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("Po", text))
}

/** A box of text with its title and a copy button: a call, its output. */
@Composable
private fun CodeBox(title: String, text: String) {
    val context = LocalContext.current
    Row(Modifier.fillMaxWidth().padding(top = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        IconButton(onClick = { copy(context, text) }, Modifier.size(32.dp)) { Icon(PoIcons.Copy, stringResource(R.string.copy), Modifier.size(18.dp)) }
    }
    Surface(Modifier.fillMaxWidth().heightIn(max = 360.dp), shape = RoundedCornerShape(10.dp), color = LocalPo.current.work) {
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
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
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
            CodeBox(stringResource(R.string.tool_call), row.text)
            when {
                row.output.isNotBlank() -> CodeBox(stringResource(R.string.tool_output), row.output)
                row.status == ToolStatus.RUNNING -> Text(stringResource(R.string.tool_running), Modifier.padding(top = 12.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (row.detail.isNotBlank()) {
                Text(stringResource(R.string.tool_diff), Modifier.padding(top = 12.dp, bottom = 4.dp), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                DiffBox(row.detail)
            }
        }
    }
}

@Composable
private fun DiffBox(diff: String) {
    Surface(color = LocalPo.current.work, shape = RoundedCornerShape(10.dp)) {
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

/** "+": the camera, the photos, the files — several at once. */
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
                selected = currentMode == "builder" || (currentMode != "plan" && currentMode != "bypass"),
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
 * 1-click Hardware & Active Model view from anywhere in the app.
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
                .padding(bottom = 24.dp)
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

            Spacer(Modifier.height(16.dp))

            // Active Model Card
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier.fillMaxWidth()
            ) {
                Column(Modifier.padding(16.dp)) {
                    Text(
                        stringResource(R.string.hardware_model),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.primary
                    )
                    Spacer(Modifier.height(4.dp))
                    val modelName = status?.pinned ?: status?.model?.ifBlank { null } ?: "—"
                    Text(
                        modelName,
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.Bold
                    )
                    if (status?.placement != null) {
                        Spacer(Modifier.height(4.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Box(Modifier.size(8.dp).clip(CircleShape).background(PoColors.Moss))
                            Spacer(Modifier.width(6.dp))
                            Text(
                                status.placement!!,
                                style = MaterialTheme.typography.bodyMedium,
                                fontWeight = FontWeight.Medium,
                                color = PoColors.Moss
                            )
                        }
                    }
                    if (status?.engine?.isNotBlank() == true) {
                        Spacer(Modifier.height(4.dp))
                        Text(
                            "${stringResource(R.string.context_engine)}: ${status.engine}",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                    if (status?.effort != null) {
                        Spacer(Modifier.height(4.dp))
                        Text(
                            "${stringResource(R.string.context_thinking)}: ${status.effort}",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                }
            }

            Spacer(Modifier.height(16.dp))

            // GPU & Card details
            Text(
                stringResource(R.string.hardware_gpu),
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(bottom = 8.dp)
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
                        "Aucun GPU dédié détecté (inférence CPU).",
                        Modifier.padding(14.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }
            } else {
                usage.gpus.forEachIndexed { idx, gpu ->
                    Surface(
                        shape = RoundedCornerShape(14.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        modifier = Modifier.fillMaxWidth().padding(bottom = 10.dp)
                    ) {
                        Column(Modifier.padding(14.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    "GPU $idx · ${gpu.name}",
                                    style = MaterialTheme.typography.bodyMedium,
                                    fontWeight = FontWeight.SemiBold,
                                    modifier = Modifier.weight(1f)
                                )
                                gpu.temperatureC?.let {
                                    Text(
                                        "$it °C",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = if (it > 80u) PoColors.Red else MaterialTheme.colorScheme.onSurfaceVariant
                                    )
                                }
                            }
                            Spacer(Modifier.height(8.dp))
                            val vramTotal = gpu.vramTotalMib.toLong().coerceAtLeast(1)
                            val vramUsed = gpu.vramUsedMib.toLong()
                            val frac = vramUsed.toFloat() / vramTotal
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    stringResource(R.string.context_vram),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.weight(1f)
                                )
                                Text(
                                    stringResource(R.string.context_gb_of, vramUsed / 1024.0, vramTotal / 1024.0) + " (${(frac * 100).toInt()} %)",
                                    style = MaterialTheme.typography.bodySmall,
                                    fontWeight = FontWeight.Medium
                                )
                            }
                            Spacer(Modifier.height(4.dp))
                            UsageBar(frac, height = 8)

                            Spacer(Modifier.height(8.dp))
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    "Charge GPU",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.weight(1f)
                                )
                                Text(
                                    "${gpu.percent.toInt()} %",
                                    style = MaterialTheme.typography.bodySmall,
                                    fontWeight = FontWeight.Medium
                                )
                            }
                            Spacer(Modifier.height(4.dp))
                            UsageBar(gpu.percent.toFloat() / 100f, height = 6)
                        }
                    }
                }
            }

            Spacer(Modifier.height(8.dp))

            // System Resources (CPU & RAM)
            if (usage != null) {
                Text(
                    "Système hôte",
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(bottom = 8.dp)
                )
                InfoRow(stringResource(R.string.context_cpu), "${usage.cpuPercent.toInt()} %")
                UsageBar(usage.cpuPercent / 100f, height = 6)
                Spacer(Modifier.height(6.dp))
                val ramTotal = usage.ramTotalMib.toLong().coerceAtLeast(1)
                InfoRow(stringResource(R.string.context_ram), stringResource(R.string.context_gb_of, usage.ramUsedMib.toLong() / 1024.0, ramTotal / 1024.0))
                UsageBar(usage.ramUsedMib.toLong().toFloat() / ramTotal, height = 6)
            }

            Spacer(Modifier.height(20.dp))

            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Button(
                    onClick = {
                        onClose()
                        state.command("/profil")
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

/** The models of the session, with 1-click thinking effort selector at top. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ModelSheet(state: AppState, pinned: String?, models: List<String>, onClose: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onClose) {
        Column(
            Modifier
                .navigationBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 20.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp)
        ) {
            Text(
                stringResource(R.string.model_title),
                Modifier.fillMaxWidth().padding(bottom = 2.dp),
                style = MaterialTheme.typography.titleLarge,
                textAlign = TextAlign.Center,
                fontWeight = FontWeight.Bold
            )

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

            Text(
                "Modèles disponibles",
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
                OutlinedButton(onClick = { onClose(); state.command("/profil") }, Modifier.weight(1f)) { Text(stringResource(R.string.model_settings)) }
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
                Text(name, style = MaterialTheme.typography.titleMedium)
                if (hint.isNotBlank()) Text(hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (chosen) Icon(Icons.Default.Check, null, tint = MaterialTheme.colorScheme.primary)
        }
    }
}
