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
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
        Text(value, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium, textAlign = TextAlign.End)
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
                InfoRow(stringResource(R.string.context_ram), String.format("%.1f / %.0f Go", usage.ramUsedMib.toLong() / 1024.0, ramTotal / 1024.0))
                UsageBar(usage.ramUsedMib.toLong().toFloat() / ramTotal, height = 6)
                usage.gpus.forEach { gpu ->
                    Spacer(Modifier.height(8.dp))
                    val vram = gpu.vramTotalMib.toLong().coerceAtLeast(1)
                    val temp = gpu.temperatureC?.let { " · ${it} °C" } ?: ""
                    InfoRow(gpu.name, "${gpu.percent.toInt()} %$temp")
                    InfoRow(stringResource(R.string.context_vram), String.format("%.1f / %.0f Go", gpu.vramUsedMib.toLong() / 1024.0, vram / 1024.0))
                    UsageBar(gpu.vramUsedMib.toLong().toFloat() / vram, height = 6)
                }
                if (usage.disks.isNotEmpty()) {
                    Spacer(Modifier.height(8.dp))
                    usage.disks.forEach { disk ->
                        InfoRow(stringResource(R.string.context_disk, disk.name), String.format("%.0f Go libres / %.0f", disk.freeGib, disk.totalGib))
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

/** The models of the session, as cards; the one chosen ticked. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ModelSheet(state: AppState, pinned: String?, models: List<String>, onClose: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onClose) {
        Column(Modifier.navigationBarsPadding().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).padding(bottom = 20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.model_title), Modifier.fillMaxWidth().padding(bottom = 6.dp), style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
            ModelCard(stringResource(R.string.model_auto), stringResource(R.string.model_auto_hint), pinned == null) { onClose(); state.send(InputRec.Command("/model auto")) }
            models.forEach { m -> ModelCard(m, "", m == pinned) { onClose(); state.send(InputRec.Command("/model $m")) } }
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
                Text(name, style = MaterialTheme.typography.titleMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                if (hint.isNotBlank()) Text(hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (chosen) Icon(Icons.Default.Check, null, tint = MaterialTheme.colorScheme.primary)
        }
    }
}
