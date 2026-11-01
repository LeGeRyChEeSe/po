package dev.po.remote.ui

import android.app.Activity
import android.content.Intent
import android.speech.RecognizerIntent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import dev.po.remote.AppState
import dev.po.remote.R
import dev.po.remote.Screen
import uniffi.po_mobile.InputRec
import uniffi.po_mobile.ItemKind
import uniffi.po_mobile.ItemRec
import uniffi.po_mobile.ToolStatus

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationScreen(state: AppState, screen: Screen.Conversation, snackbar: SnackbarHostState) {
    val status = state.status
    val list = rememberLazyListState()
    var commandsOpen by remember { mutableStateOf(false) }
    var overflow by remember { mutableStateOf(false) }
    val readOnly = screen.key.startsWith("saved:")
    // The end follows the conversation, as long as the user has not scrolled up.
    LaunchedEffect(state.rows.size, state.rows.lastOrNull()?.text?.length) {
        val last = state.rows.size - 1
        val atEnd = list.layoutInfo.visibleItemsInfo.lastOrNull()?.index?.let { it >= last - 2 } ?: true
        if (last >= 0 && atEnd) list.animateScrollToItem(last)
    }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(status?.title?.takeIf { it.isNotBlank() } ?: stringResource(R.string.untitled), maxLines = 1, overflow = TextOverflow.Ellipsis)
                        val sub = listOfNotNull(
                            status?.model?.takeIf { it.isNotBlank() },
                            status?.workMode?.takeIf { it.isNotBlank() },
                            status?.takeIf { it.contextWindow > 0u }?.let { "${(it.contextUsed.toLong() * 100 / it.contextWindow.toLong())} %" },
                            if (status?.labs == true) "Po Labs" else null,
                        ).joinToString(" · ")
                        if (sub.isNotEmpty()) Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                    }
                },
                navigationIcon = { IconButton(onClick = { state.back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
                actions = {
                    if (!readOnly) IconButton(onClick = { commandsOpen = true }) { Icon(Icons.Default.Menu, stringResource(R.string.commands)) }
                    IconButton(onClick = { overflow = true }) { Icon(Icons.Default.MoreVert, null) }
                    DropdownMenu(overflow, { overflow = false }) {
                        if (readOnly) {
                            DropdownMenuItem(text = { Text(stringResource(R.string.resume_here)) }, onClick = { overflow = false; state.resumeSaved(screen.pc, screen.key.removePrefix("saved:")) })
                        } else {
                            DropdownMenuItem(text = { Text(stringResource(R.string.work_mode)) }, onClick = { overflow = false; state.command("/mode") })
                            DropdownMenuItem(text = { Text("Po Labs") }, onClick = { overflow = false; state.command("/labs") })
                            DropdownMenuItem(text = { Text(stringResource(R.string.close_session)) }, onClick = { overflow = false; state.closeSession() })
                        }
                    }
                },
            )
        },
        bottomBar = {
            Column(Modifier.navigationBarsPadding().imePadding()) {
                if (state.running) Working(state)
                if (!state.closed || readOnly) InputBar(state, readOnly)
                else Text(stringResource(R.string.session_ended), Modifier.padding(16.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding), state = list, contentPadding = PaddingValues(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            if (state.rows.isEmpty()) item { Empty(if (state.running) stringResource(R.string.working) else stringResource(R.string.empty_session)) }
            itemsIndexed(state.rows, key = { i, _ -> i }) { _, row -> ConversationRow(row) }
            item { Spacer(Modifier.height(8.dp)) }
        }
    }
    state.approval?.let { approval ->
        ModalBottomSheet(onDismissRequest = {}) {
            Column(Modifier.padding(20.dp).navigationBarsPadding()) {
                Text(stringResource(R.string.approval_title), style = MaterialTheme.typography.titleLarge)
                Spacer(Modifier.height(8.dp))
                Text(approval.tool, fontWeight = FontWeight.SemiBold, color = MaterialTheme.colorScheme.primary)
                Surface(Modifier.fillMaxWidth().padding(vertical = 10.dp), color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(10.dp)) {
                    Text(approval.summary, Modifier.padding(12.dp), style = Mono)
                }
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    OutlinedButton(onClick = { state.send(InputRec.Approve(approval.id, false)) }, Modifier.weight(1f)) { Text(stringResource(R.string.deny)) }
                    Button(onClick = { state.send(InputRec.Approve(approval.id, true)) }, Modifier.weight(1f)) { Text(stringResource(R.string.allow)) }
                }
            }
        }
    }
    state.question?.let { question ->
        var free by remember(question.id) { mutableStateOf("") }
        ModalBottomSheet(onDismissRequest = {}) {
            Column(Modifier.padding(20.dp).navigationBarsPadding(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(question.question, style = MaterialTheme.typography.titleMedium)
                question.options.forEachIndexed { i, option ->
                    Card(Modifier.fillMaxWidth().clickable { state.send(InputRec.Answer(question.id, i.toUInt(), null)) }) {
                        Column(Modifier.padding(12.dp)) {
                            Text(option.label, fontWeight = FontWeight.SemiBold)
                            if (option.description.isNotBlank()) Text(option.description, style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
                OutlinedTextField(free, { free = it }, Modifier.fillMaxWidth(), placeholder = { Text(stringResource(R.string.other_answer)) })
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    TextButton(onClick = { state.send(InputRec.Answer(question.id, null, null)) }) { Text(stringResource(R.string.decline)) }
                    Spacer(Modifier.weight(1f))
                    Button(onClick = { state.send(InputRec.Answer(question.id, null, free)) }, enabled = free.isNotBlank()) { Text(stringResource(R.string.answer)) }
                }
            }
        }
    }
    if (commandsOpen) {
        ModalBottomSheet(onDismissRequest = { commandsOpen = false }) {
            LazyColumn(Modifier.navigationBarsPadding()) {
                item { Text(stringResource(R.string.commands), Modifier.padding(16.dp), style = MaterialTheme.typography.titleMedium) }
                itemsIndexed(state.commands) { _, c ->
                    Column(Modifier.fillMaxWidth().clickable { commandsOpen = false; state.command(c.name.split(' ').first()) }.padding(horizontal = 16.dp, vertical = 10.dp)) {
                        Text(c.name, style = Mono, fontWeight = FontWeight.SemiBold)
                        Text(c.description, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
    }
    state.menu?.let { menu ->
        ModalBottomSheet(onDismissRequest = { state.menu = null }) {
            LazyColumn(Modifier.navigationBarsPadding()) {
                item {
                    Column(Modifier.padding(16.dp)) {
                        Text(menu.title, style = MaterialTheme.typography.titleMedium)
                        menu.note?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                    }
                }
                itemsIndexed(menu.items) { _, item ->
                    Row(
                        Modifier.fillMaxWidth().clickable { state.chooseMenuItem(item.action, item.command) }.padding(horizontal = 16.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(if (item.current) "●" else "", Modifier.width(18.dp), color = MaterialTheme.colorScheme.primary)
                        Text(item.label, Modifier.weight(1f), fontWeight = if (item.current) FontWeight.SemiBold else FontWeight.Normal)
                        if (item.detail.isNotBlank()) Text(item.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                    }
                }
            }
        }
    }
}

@Composable
private fun Working(state: AppState) {
    Column {
        LinearProgressIndicator(Modifier.fillMaxWidth())
        Row(Modifier.padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(stringResource(R.string.working), Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = { state.send(InputRec.Interrupt) }) { Text(stringResource(R.string.stop)) }
        }
    }
}

@Composable
private fun InputBar(state: AppState, readOnly: Boolean) {
    val status = state.status
    var models by remember { mutableStateOf(false) }
    val listen = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == Activity.RESULT_OK) {
            val heard = result.data?.getStringArrayListExtra(RecognizerIntent.EXTRA_RESULTS)?.firstOrNull()
            if (!heard.isNullOrBlank()) state.draft = (state.draft.trimEnd() + " " + heard).trim()
        }
    }
    Surface(tonalElevation = 3.dp) {
        Column(Modifier.padding(horizontal = 10.dp, vertical = 6.dp)) {
            if (!readOnly && status != null && status.models.isNotEmpty()) {
                Box {
                    AssistChip(onClick = { models = true }, label = { Text(status.pinned ?: (status.model.ifBlank { stringResource(R.string.model_auto) }), maxLines = 1) })
                    DropdownMenu(models, { models = false }) {
                        DropdownMenuItem(text = { Text(stringResource(R.string.model_auto)) }, onClick = { models = false; state.send(InputRec.Command("/model auto")) })
                        status.models.forEach { m ->
                            DropdownMenuItem(
                                text = { Text(m, fontWeight = if (m == status.pinned) FontWeight.Bold else FontWeight.Normal) },
                                onClick = { models = false; state.send(InputRec.Command("/model $m")) },
                            )
                        }
                    }
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    state.draft,
                    { state.draft = it },
                    Modifier.weight(1f),
                    placeholder = { Text(stringResource(if (readOnly) R.string.write_to_resume else R.string.write)) },
                    maxLines = 6,
                    shape = RoundedCornerShape(22.dp),
                )
                IconButton(onClick = {
                    val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
                        .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
                    runCatching { listen.launch(intent) }.onFailure { state.toast = it.message }
                }) { MicIcon() }
                IconButton(onClick = { state.submit() }, enabled = state.draft.isNotBlank()) { Icon(Icons.AutoMirrored.Filled.Send, stringResource(R.string.send)) }
            }
        }
    }
}

/** A microphone drawn with text, to avoid the large extended icon set. */
@Composable
private fun MicIcon() {
    Text("🎙", style = MaterialTheme.typography.titleMedium)
}

@Composable
private fun ConversationRow(row: ItemRec) {
    when (row.kind) {
        ItemKind.USER -> Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
            Surface(color = MaterialTheme.colorScheme.primaryContainer, shape = RoundedCornerShape(18.dp, 18.dp, 4.dp, 18.dp)) {
                Text(row.text, Modifier.padding(horizontal = 14.dp, vertical = 10.dp).widthIn(max = 320.dp))
            }
        }
        ItemKind.ASSISTANT -> RichText(row.text + if (row.streaming) " ▍" else "")
        ItemKind.THINKING -> Folded(
            title = stringResource(R.string.reasoning, (row.chars / 4u).toInt()) + if (row.streaming) " …" else "",
            body = row.text,
            italic = true,
        )
        ItemKind.TOOL -> ToolRow(row)
        ItemKind.ASKED -> Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant)) {
            Column(Modifier.padding(12.dp)) {
                Text(row.text, style = MaterialTheme.typography.bodyMedium)
                Text("→ ${row.detail}", fontWeight = FontWeight.SemiBold)
            }
        }
        ItemKind.DELEGATION -> Folded(title = "⇢ ${row.title}" + if (row.streaming) " · " + stringResource(R.string.background) else "", body = row.detail, italic = false)
        ItemKind.NOTICE -> Text(row.text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, fontStyle = FontStyle.Italic)
        ItemKind.WARNING -> Text(row.text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        ItemKind.OUTPUT -> Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer)) {
            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(row.title, style = Mono, fontWeight = FontWeight.SemiBold)
                row.lines.forEach { l ->
                    when (l.kind) {
                        "section" -> Text(l.text, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(top = 4.dp))
                        "warn" -> Text(l.text, color = MaterialTheme.colorScheme.error)
                        "dim" -> Text(l.text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        "item" -> Text(buildAnnotatedString { withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(l.text) }; if (l.detail.isNotBlank()) append(" — ${l.detail}") }, style = MaterialTheme.typography.bodySmall)
                        "field" -> Text(buildAnnotatedString { append("${l.text} : "); withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(l.detail) } }, style = MaterialTheme.typography.bodySmall)
                        else -> Text(l.text, style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
        }
        ItemKind.SUMMARY -> Text(row.text, Modifier.fillMaxWidth().padding(vertical = 2.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun ToolRow(row: ItemRec) {
    var open by remember { mutableStateOf(false) }
    val (mark, color) = when (row.status) {
        ToolStatus.RUNNING -> "" to MaterialTheme.colorScheme.onSurfaceVariant
        ToolStatus.ERROR -> "✕" to MaterialTheme.colorScheme.error
        ToolStatus.DENIED -> "⊘" to MaterialTheme.colorScheme.error
        else -> "✓" to MaterialTheme.colorScheme.tertiary
    }
    Column(Modifier.fillMaxWidth().animateContentSize()) {
        Row(Modifier.fillMaxWidth().clickable(enabled = row.detail.isNotBlank()) { open = !open }.padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            if (row.status == ToolStatus.RUNNING) CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp) else Text(mark, color = color)
            Spacer(Modifier.width(8.dp))
            Text(row.title, style = Mono, fontWeight = FontWeight.SemiBold, color = color)
            Spacer(Modifier.width(6.dp))
            Text(row.text, Modifier.weight(1f), style = Mono, maxLines = if (open) 6 else 1, overflow = TextOverflow.Ellipsis, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (row.detail.isNotBlank()) Text(if (open) "▴" else "▾", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (open && row.detail.isNotBlank()) Diff(row.detail)
    }
}

@Composable
private fun Diff(diff: String) {
    Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
        Column(Modifier.horizontalScroll(rememberScrollState()).padding(8.dp)) {
            diff.lines().take(400).forEach { line ->
                val (fg, bg) = when {
                    line.startsWith("+") -> PoColors.Moss to PoColors.Moss.copy(alpha = 0.10f)
                    line.startsWith("-") -> PoColors.Red to PoColors.Red.copy(alpha = 0.10f)
                    line.startsWith("@@") -> MaterialTheme.colorScheme.primary to Color.Transparent
                    else -> MaterialTheme.colorScheme.onSurface to Color.Transparent
                }
                Text(line, Modifier.background(bg), style = Mono, color = fg, softWrap = false)
            }
        }
    }
}

@Composable
private fun Folded(title: String, body: String, italic: Boolean) {
    var open by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().animateContentSize()) {
        Text(
            (if (open) "▾ " else "▸ ") + title,
            Modifier.clickable(enabled = body.isNotBlank()) { open = !open }.padding(vertical = 4.dp),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (open) Text(body, Modifier.padding(start = 14.dp, bottom = 4.dp), style = MaterialTheme.typography.bodySmall, fontStyle = if (italic) FontStyle.Italic else FontStyle.Normal, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** An answer: code blocks in a monospace box, `inline code` and **bold** marked, the rest as is. */
@Composable
private fun RichText(text: String) {
    val parts = text.split("```")
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        parts.forEachIndexed { i, part ->
            if (i % 2 == 1) {
                val code = part.substringAfter('\n', part)
                Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
                    Text(code.trimEnd(), Modifier.horizontalScroll(rememberScrollState()).padding(10.dp), style = Mono, softWrap = false)
                }
            } else if (part.isNotBlank()) {
                Text(inline(part.trim('\n')))
            }
        }
    }
}

private fun inline(text: String) = buildAnnotatedString {
    var i = 0
    while (i < text.length) {
        when {
            text.startsWith("**", i) && text.indexOf("**", i + 2) > 0 -> {
                val end = text.indexOf("**", i + 2)
                withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(text.substring(i + 2, end)) }
                i = end + 2
            }
            text[i] == '`' && text.indexOf('`', i + 1) > 0 -> {
                val end = text.indexOf('`', i + 1)
                withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = Color(0x22888888))) { append(text.substring(i + 1, end)) }
                i = end + 1
            }
            else -> {
                append(text[i]); i++
            }
        }
    }
}
