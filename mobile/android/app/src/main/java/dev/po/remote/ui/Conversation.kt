package dev.po.remote.ui

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.material.icons.filled.Add
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
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
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import dev.po.remote.AppState
import dev.po.remote.OPENING
import dev.po.remote.R
import dev.po.remote.Screen
import kotlinx.coroutines.delay
import uniffi.po_mobile.InputRec
import uniffi.po_mobile.ItemKind
import uniffi.po_mobile.ItemRec
import uniffi.po_mobile.LinkState
import uniffi.po_mobile.MenuRec
import uniffi.po_mobile.SubagentRec
import uniffi.po_mobile.ToolStatus

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationScreen(state: AppState, screen: Screen.Conversation, snackbar: SnackbarHostState) {
    val status = state.status
    val list = rememberLazyListState()
    var overflow by remember { mutableStateOf(false) }
    var agentsOpen by remember { mutableStateOf(false) }
    var agentShown by remember { mutableStateOf<String?>(null) }
    var contextOpen by remember { mutableStateOf(false) }
    var toolShown by remember { mutableStateOf<ItemRec?>(null) }
    val readOnly = screen.key.startsWith("saved:")
    val link = state.links[screen.pc]
    // The end follows the conversation, as long as the user has not scrolled up.
    // What the user just sent: the end, whatever was being read.
    LaunchedEffect(state.sent) {
        if (state.sent > 0) {
            delay(300)
            val last = state.rows.size
            if (last > 0) list.animateScrollToItem(last)
        }
    }
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
                        Text(status?.title?.takeIf { it.isNotBlank() } ?: stringResource(if (screen.key.startsWith(OPENING)) R.string.new_session else R.string.untitled), maxLines = 1, overflow = TextOverflow.Ellipsis)
                        val sub = listOfNotNull(
                            status?.model?.takeIf { it.isNotBlank() },
                            if (status?.labs == true) "Po Labs" else null,
                        ).joinToString(" · ")
                        if (sub.isNotEmpty()) Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                },
                navigationIcon = { IconButton(onClick = { state.back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
                actions = {
                    if (state.subagents.isNotEmpty()) SubagentsChip(state.subagents) { agentsOpen = true }
                    if (status != null && status.contextWindow > 0u) ContextGauge(status.contextUsed.toLong(), status.contextWindow.toLong()) { contextOpen = true }
                    IconButton(onClick = { overflow = true }) { Icon(Icons.Default.MoreVert, null) }
                    DropdownMenu(overflow, { overflow = false }) {
                        if (readOnly) {
                            DropdownMenuItem(text = { Text(stringResource(R.string.resume_here)) }, onClick = { overflow = false; state.resumeSaved(screen.pc, screen.key.removePrefix("saved:")) })
                        } else {
                            DropdownMenuItem(text = { Text(stringResource(R.string.commands)) }, onClick = { overflow = false; state.draft = "/" })
                            DropdownMenuItem(text = { Text(stringResource(R.string.model_settings)) }, onClick = { overflow = false; state.command("/profil") })
                            DropdownMenuItem(text = { Text(stringResource(R.string.local_models)) }, onClick = { overflow = false; state.command("/configurer") })
                            DropdownMenuItem(text = { Text(stringResource(R.string.close_session)) }, onClick = { overflow = false; state.closeSession() })
                        }
                        DropdownMenuItem(text = { Text(stringResource(R.string.app_settings)) }, onClick = { overflow = false; state.go(Screen.Settings) })
                    }
                },
            )
        },
        bottomBar = {
            Column(Modifier.navigationBarsPadding().imePadding()) {
                if (!state.closed || readOnly) {
                    Queue(state)
                    Suggestions(state)
                    Composer(state, readOnly)
                } else {
                    Text(stringResource(R.string.session_ended), Modifier.padding(16.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            if (state.loading) {
                Loading(screen, link)
            } else {
                LazyColumn(Modifier.fillMaxSize(), state = list, contentPadding = PaddingValues(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (state.rows.isEmpty()) item { Empty(stringResource(if (state.running) R.string.working else R.string.empty_session)) }
                    itemsIndexed(state.rows, key = { i, _ -> i }) { _, row ->
                        ConversationRow(state, row, onDelegation = { agent ->
                            agentShown = state.subagents.lastOrNull { it.agent == agent }?.id
                            if (agentShown == null && state.subagents.isNotEmpty()) agentsOpen = true
                        }, onTool = { toolShown = it })
                    }
                    // Working, with nothing being written yet (the model thinks, a tool runs): said
                    // discreetly at the end, where the answer will come.
                    if (state.running && state.rows.lastOrNull()?.streaming != true) item { Working(state.status?.paused == true) }
                    item { Spacer(Modifier.height(8.dp)) }
                }
            }
            if (!state.loading && (link is LinkState.Connecting || link is LinkState.Down)) Reconnecting(Modifier.align(Alignment.TopCenter))
            if (state.menuLoading && state.menu == null) LinearProgressIndicator(Modifier.fillMaxWidth().align(Alignment.TopCenter))
        }
    }
    state.approval?.let { approval ->
        ModalBottomSheet(onDismissRequest = {}) {
            Column(Modifier.padding(20.dp).navigationBarsPadding()) {
                Text(stringResource(R.string.approval_title), style = MaterialTheme.typography.titleLarge)
                Spacer(Modifier.height(8.dp))
                Text(approval.tool, fontWeight = FontWeight.SemiBold, color = MaterialTheme.colorScheme.primary)
                Surface(Modifier.fillMaxWidth().padding(vertical = 10.dp), color = LocalPo.current.work, shape = RoundedCornerShape(10.dp)) {
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
    state.menu?.let { menu -> MenuSheet(state, menu) }
    if (contextOpen) ContextSheet(state) { contextOpen = false }
    toolShown?.let { ToolSheet(it) { toolShown = null } }
    if (agentsOpen) SubagentsSheet(state.subagents, onClose = { agentsOpen = false }) { agentsOpen = false; agentShown = it }
    agentShown?.let { id -> SubagentView(state, id) { agentShown = null } }
}

/** Loading the conversation: what is being waited for, and for how long. */
@Composable
private fun Loading(screen: Screen.Conversation, link: LinkState?) {
    var seconds by remember { mutableIntStateOf(0) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1000)
            seconds += 1
        }
    }
    val stage = when {
        screen.key.startsWith(OPENING) -> stringResource(R.string.loading_starting)
        link is LinkState.Up -> stringResource(R.string.loading_conversation)
        link is LinkState.Down -> stringResource(R.string.loading_retry)
        else -> stringResource(R.string.loading_connecting)
    }
    Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
        CircularProgressIndicator(Modifier.size(42.dp), strokeWidth = 3.dp)
        Spacer(Modifier.height(16.dp))
        Text(stage, style = MaterialTheme.typography.bodyLarge)
        if (seconds > 0) Text(stringResource(R.string.loading_seconds, seconds), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun Working(paused: Boolean) {
    Row(Modifier.padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        if (paused) Icon(PoIcons.Pause, null, Modifier.size(14.dp), tint = LocalPo.current.warn)
        else CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp)
        Spacer(Modifier.width(10.dp))
        Text(stringResource(if (paused) R.string.pausing else R.string.working), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, fontStyle = FontStyle.Italic)
    }
}

@Composable
private fun Reconnecting(modifier: Modifier) {
    Surface(modifier.padding(top = 6.dp), shape = CircleShape, color = MaterialTheme.colorScheme.secondaryContainer, shadowElevation = 3.dp) {
        Row(Modifier.padding(horizontal = 14.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
            Spacer(Modifier.width(8.dp))
            Text(stringResource(R.string.reconnecting), style = MaterialTheme.typography.labelMedium)
        }
    }
}

/** How full the context window is: a ring from green to red, the percentage inside. */
@Composable
/** Green while there is room, through amber, to red near the end of the window. */
fun gaugeColor(fraction: Float): Color = when {
    fraction < 0.5f -> lerp(Color(0xFF3BB273), Color(0xFFE0B23A), fraction / 0.5f)
    else -> lerp(Color(0xFFE0B23A), Color(0xFFE5484D), ((fraction - 0.5f) / 0.4f).coerceIn(0f, 1f))
}

@Composable
private fun ContextGauge(used: Long, window: Long, onClick: () -> Unit) {
    val fraction = (used.toFloat() / window.toFloat()).coerceIn(0f, 1f)
    val color = gaugeColor(fraction)
    Box(Modifier.padding(horizontal = 4.dp).size(38.dp).clip(CircleShape).clickable(onClick = onClick), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(progress = { fraction }, modifier = Modifier.size(32.dp), color = color, trackColor = color.copy(alpha = 0.18f), strokeWidth = 3.5.dp)
        Text("${(fraction * 100).toInt()}", style = MaterialTheme.typography.labelSmall, fontWeight = FontWeight.Bold, color = color)
    }
}

@Composable
private fun SubagentsChip(subagents: List<SubagentRec>, onClick: () -> Unit) {
    val running = subagents.count { it.state == "running" }
    Surface(Modifier.padding(horizontal = 2.dp).clip(CircleShape).clickable(onClick = onClick), shape = CircleShape, color = MaterialTheme.colorScheme.secondaryContainer) {
        Row(Modifier.padding(horizontal = 10.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            if (running > 0) CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp)
            else Icon(PoIcons.Agents, null, Modifier.size(16.dp))
            Spacer(Modifier.width(6.dp))
            Text(if (running > 0) "$running" else "${subagents.size}", style = MaterialTheme.typography.labelLarge)
        }
    }
}

/** The messages waiting for the model: each can be changed or taken back. */
@Composable
private fun Queue(state: AppState) {
    val queue = state.status?.queue.orEmpty()
    if (queue.isEmpty()) return
    var editing by remember { mutableStateOf<Pair<Int, String>?>(null) }
    Column(Modifier.fillMaxWidth().padding(horizontal = 10.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        queue.forEachIndexed { i, text ->
            Surface(shape = RoundedCornerShape(14.dp), color = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.6f)) {
                Row(Modifier.fillMaxWidth().padding(start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f).padding(vertical = 6.dp)) {
                        Text(stringResource(R.string.queued), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                        Text(text, maxLines = 2, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium)
                    }
                    IconButton(onClick = { editing = i to text }) { Icon(Icons.Default.Edit, stringResource(R.string.edit), Modifier.size(20.dp)) }
                    IconButton(onClick = { state.unqueue(i) }) { Icon(Icons.Default.Delete, stringResource(R.string.remove), Modifier.size(20.dp)) }
                }
            }
        }
    }
    editing?.let { (index, initial) ->
        var text by remember(index, initial) { mutableStateOf(initial) }
        AlertDialog(
            onDismissRequest = { editing = null },
            title = { Text(stringResource(R.string.edit_queued)) },
            text = { OutlinedTextField(text, { text = it }, Modifier.fillMaxWidth(), maxLines = 8) },
            confirmButton = { Button(onClick = { state.requeue(index, text); editing = null }, enabled = text.isNotBlank()) { Text(stringResource(R.string.save)) } },
            dismissButton = { TextButton(onClick = { editing = null }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

/** The commands, as soon as a slash is typed: one touch puts it in the input. */
@Composable
private fun Suggestions(state: AppState) {
    val typed = state.draft
    if (!typed.startsWith("/") || typed.contains(' ') || typed.contains('\n')) return
    val word = typed.lowercase()
    val shown = state.commands.filter { it.name.lowercase().startsWith(word) }.distinctBy { it.name.split(' ').first() }
    if (shown.isEmpty()) return
    Surface(Modifier.fillMaxWidth().padding(horizontal = 10.dp).heightIn(max = 260.dp), shape = RoundedCornerShape(16.dp), color = MaterialTheme.colorScheme.surfaceVariant, shadowElevation = 2.dp) {
        LazyColumn(contentPadding = PaddingValues(vertical = 4.dp)) {
            itemsIndexed(shown) { _, c ->
                val name = c.name.split(' ').first()
                Column(Modifier.fillMaxWidth().clickable { state.draft = "$name " }.padding(horizontal = 14.dp, vertical = 8.dp)) {
                    Text(c.name, style = Mono, fontWeight = FontWeight.SemiBold, color = MaterialTheme.colorScheme.primary, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if (c.description.isNotBlank()) Text(c.description, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
            }
        }
    }
}

/** The input: the text over the whole width, and under it every button, in the same frame. */
@Composable
private fun Composer(state: AppState, readOnly: Boolean) {
    val context = LocalContext.current
    val status = state.status
    var attachMenu by remember { mutableStateOf(false) }
    var photo by remember { mutableStateOf<android.net.Uri?>(null) }
    val pickMedia = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(20)) { uris -> uris.forEach(state::attach) }
    // The text with its cursor: a command chosen in the list leaves the cursor after it.
    var field by remember { mutableStateOf(TextFieldValue(state.draft)) }
    LaunchedEffect(state.draft) {
        if (field.text != state.draft) field = TextFieldValue(state.draft, TextRange(state.draft.length))
    }
    val pickFile = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris -> uris.forEach(state::attach) }
    val takePhoto = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { ok -> photo?.takeIf { ok }?.let(state::attach) }
    val micAllowed = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted -> if (granted) state.toggleRecording(context) }
    val hasText = state.draft.isNotBlank() || state.attachments.isNotEmpty()
    val paused = status?.paused == true
    Surface(Modifier.fillMaxWidth().padding(8.dp), shape = RoundedCornerShape(24.dp), color = MaterialTheme.colorScheme.surfaceVariant, tonalElevation = 2.dp) {
        Column(Modifier.padding(start = 6.dp, end = 6.dp, top = 10.dp, bottom = 4.dp)) {
            if (state.attachments.isNotEmpty() || state.uploading > 0) {
                Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 8.dp, vertical = 2.dp), horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                    state.attachments.forEach { a ->
                        Surface(shape = CircleShape, color = MaterialTheme.colorScheme.primaryContainer) {
                            Row(Modifier.padding(start = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                                Icon(if (a.image) PoIcons.Image else PoIcons.File, null, Modifier.size(16.dp))
                                Spacer(Modifier.width(4.dp))
                                Text(a.name, style = MaterialTheme.typography.labelMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 160.dp))
                                IconButton(onClick = { state.detach(a) }, Modifier.size(30.dp)) { Icon(Icons.Default.Close, stringResource(R.string.remove), Modifier.size(14.dp)) }
                            }
                        }
                    }
                    if (state.uploading > 0) {
                        CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                        Text(stringResource(R.string.uploading), style = MaterialTheme.typography.labelMedium)
                    }
                }
            }
            BasicTextField(
                value = field,
                onValueChange = {
                    field = it
                    state.draft = it.text
                },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp).heightIn(min = 26.dp, max = 180.dp),
                textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
                cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                decorationBox = { inner ->
                    Box {
                        if (state.draft.isEmpty()) Text(stringResource(if (readOnly) R.string.write_to_resume else R.string.write), style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        inner()
                    }
                },
            )
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = { attachMenu = true }) { Icon(Icons.Default.Add, stringResource(R.string.attach), Modifier.size(24.dp)) }
                if (attachMenu) {
                    AttachSheet(
                        onCamera = {
                            val dir = java.io.File(context.cacheDir, "camera").apply { mkdirs() }
                            val file = java.io.File(dir, "photo-${System.currentTimeMillis()}.jpg")
                            photo = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
                            runCatching { takePhoto.launch(photo!!) }.onFailure { state.toast = it.message }
                        },
                        onPhotos = { pickMedia.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo)) },
                        onFiles = { pickFile.launch(arrayOf("*/*")) },
                        onClose = { attachMenu = false },
                    )
                }
                // The chips take what room is left: the buttons on the right always show whole.
                Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                    if (!readOnly && status != null) {
                        ModeChip(state, status.workMode)
                        if (status.models.isNotEmpty()) ModelChip(state, status.pinned, status.model, status.models, Modifier.weight(1f, fill = false))
                    }
                }
                MicButton(state) {
                    if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) state.toggleRecording(context)
                    else micAllowed.launch(Manifest.permission.RECORD_AUDIO)
                }
                if (state.running || paused) {
                    IconButton(onClick = { if (paused) state.resume() else state.pause() }) {
                        Icon(if (paused) PoIcons.Play else PoIcons.Pause, stringResource(if (paused) R.string.resume else R.string.pause), Modifier.size(22.dp))
                    }
                }
                val stop = state.running && !hasText
                FilledIconButton(
                    onClick = { if (stop) state.stop() else state.submit() },
                    enabled = stop || hasText,
                    modifier = Modifier.size(40.dp),
                    colors = IconButtonDefaults.filledIconButtonColors(
                        containerColor = if (stop) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.primary,
                        contentColor = if (stop) MaterialTheme.colorScheme.surface else MaterialTheme.colorScheme.onPrimary,
                    ),
                ) { Icon(if (stop) PoIcons.Stop else PoIcons.Send, stringResource(if (stop) R.string.stop else R.string.send), Modifier.size(20.dp)) }
            }
        }
    }
}

@Composable
private fun MicButton(state: AppState, onClick: () -> Unit) {
    when {
        state.transcribing -> Box(Modifier.size(48.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) }
        state.recording -> {
            val pulse by rememberInfiniteTransition(label = "mic").animateFloat(0.35f, 1f, infiniteRepeatable(tween(700), RepeatMode.Reverse), label = "pulse")
            IconButton(onClick = onClick) {
                Box(Modifier.size(34.dp).clip(CircleShape).background(MaterialTheme.colorScheme.error.copy(alpha = pulse * 0.9f)), contentAlignment = Alignment.Center) {
                    Icon(PoIcons.Mic, stringResource(R.string.dictate_stop), Modifier.size(20.dp), tint = Color.White)
                }
            }
        }
        else -> IconButton(onClick = onClick) { Icon(PoIcons.Mic, stringResource(R.string.dictate), Modifier.size(22.dp)) }
    }
}

/** Plan, builder, bypass: changed at any moment, even during a turn. */
@Composable
private fun ModeChip(state: AppState, mode: String) {
    var open by remember { mutableStateOf(false) }
    val (label, color) = when (mode) {
        "plan" -> stringResource(R.string.mode_plan) to Color(0xFF4F8EF7)
        "bypass" -> stringResource(R.string.mode_bypass) to MaterialTheme.colorScheme.error
        else -> stringResource(R.string.mode_build) to LocalPo.current.ok
    }
    Box {
        Surface(Modifier.clip(CircleShape).clickable { open = true }, shape = CircleShape, color = color.copy(alpha = 0.15f)) {
            Row(Modifier.padding(horizontal = 10.dp, vertical = 5.dp), verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(8.dp).clip(CircleShape).background(color))
                Spacer(Modifier.width(6.dp))
                Text(label, style = MaterialTheme.typography.labelLarge, color = color)
            }
        }
        DropdownMenu(open, { open = false }) {
            listOf("/plan" to R.string.mode_plan_hint, "/builder" to R.string.mode_build_hint, "/bypass" to R.string.mode_bypass_hint).forEach { (command, hint) ->
                DropdownMenuItem(
                    text = { Text(stringResource(hint)) },
                    trailingIcon = { if (command.removePrefix("/") == mode) Icon(Icons.Default.Check, null) },
                    onClick = { open = false; state.send(InputRec.Command(command)) },
                )
            }
        }
    }
}

@Composable
private fun ModelChip(state: AppState, pinned: String?, model: String, models: List<String>, modifier: Modifier = Modifier) {
    var open by remember { mutableStateOf(false) }
    Box(modifier.padding(start = 6.dp)) {
        Surface(Modifier.clip(CircleShape).clickable { open = true }, shape = CircleShape, color = MaterialTheme.colorScheme.background) {
            Text(
                pinned ?: model.ifBlank { stringResource(R.string.model_auto) },
                Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
                style = MaterialTheme.typography.labelLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
    if (open) ModelSheet(state, pinned, models) { open = false }
}

/** A command's menu, as the terminal's: walked by touch, Back to the menu above, live if it is. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MenuSheet(state: AppState, menu: MenuRec) {
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    LaunchedEffect(menu.live, state.menuLine) {
        while (menu.live) {
            delay(1000)
            state.refreshMenu()
        }
    }
    ModalBottomSheet(onDismissRequest = { state.closeMenu() }, sheetState = sheet) {
        Column(Modifier.navigationBarsPadding().padding(bottom = 8.dp)) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                if (menu.back != null) IconButton(onClick = { state.menuBack() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) }
                else Spacer(Modifier.width(12.dp))
                Text(menu.title, Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                IconButton(onClick = { state.closeMenu() }) { Icon(Icons.Default.Close, stringResource(R.string.close)) }
            }
            menu.note?.let { note ->
                Surface(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp), shape = RoundedCornerShape(12.dp), color = LocalPo.current.work) {
                    Row(Modifier.padding(12.dp), verticalAlignment = Alignment.Top) {
                        if (menu.busy) {
                            CircularProgressIndicator(Modifier.padding(top = 2.dp).size(16.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(10.dp))
                        }
                        SelectionContainer { Text(rich(note, LocalPo.current.link), style = MaterialTheme.typography.bodyMedium) }
                    }
                }
            }
            LazyColumn(Modifier.heightIn(max = 520.dp), contentPadding = PaddingValues(vertical = 6.dp)) {
                itemsIndexed(menu.items) { _, item ->
                    Row(
                        Modifier.fillMaxWidth().clickable { state.chooseMenuItem(item.action, item.command, item.then) }.padding(horizontal = 16.dp, vertical = 13.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Box(Modifier.width(28.dp)) {
                            if (item.current) Icon(Icons.Default.Check, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.primary)
                        }
                        Column(Modifier.weight(1f)) {
                            Text(item.label, style = MaterialTheme.typography.bodyLarge, fontWeight = if (item.current) FontWeight.SemiBold else FontWeight.Normal)
                            if (item.detail.isNotBlank()) Text(item.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        when (item.action) {
                            "fill" -> Icon(Icons.Default.Edit, null, Modifier.size(18.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                            "run" -> if (item.then == null) Icon(PoIcons.Chevron, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SubagentsSheet(subagents: List<SubagentRec>, onClose: () -> Unit, onOpen: (String) -> Unit) {
    ModalBottomSheet(onDismissRequest = onClose) {
        LazyColumn(Modifier.navigationBarsPadding(), contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            item { Text(stringResource(R.string.subagents), style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(bottom = 4.dp)) }
            itemsIndexed(subagents.reversed()) { _, s ->
                Surface(Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).clickable { onOpen(s.id) }, shape = RoundedCornerShape(14.dp), color = LocalPo.current.work) {
                    Row(Modifier.padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
                        SubagentMark(s.state)
                        Spacer(Modifier.width(10.dp))
                        Column(Modifier.weight(1f)) {
                            Text(listOf(s.agent, s.model).filter { it.isNotBlank() }.joinToString(" · "), fontWeight = FontWeight.SemiBold)
                            Text(s.progress.ifBlank { s.task.lineSequence().firstOrNull().orEmpty() }, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        }
                        Text(stateLabel(s.state), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
    }
}

@Composable
private fun SubagentMark(state: String) {
    when (state) {
        "running" -> CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
        "paused" -> Icon(PoIcons.Pause, null, Modifier.size(16.dp), tint = LocalPo.current.warn)
        "failed" -> Icon(Icons.Default.Close, null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.error)
        else -> Icon(Icons.Default.Check, null, Modifier.size(16.dp), tint = LocalPo.current.ok)
    }
}

@Composable
private fun stateLabel(state: String) = stringResource(
    when (state) {
        "running" -> R.string.agent_running
        "paused" -> R.string.agent_paused
        "failed" -> R.string.agent_failed
        else -> R.string.agent_done
    }
)

/** A subagent in full: its conversation, as the PC's big card; arrows go to the others. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SubagentView(state: AppState, id: String, onClose: () -> Unit) {
    var current by remember(id) { mutableStateOf(id) }
    val all = state.subagents
    val index = all.indexOfFirst { it.id == current }
    val sub = all.getOrNull(index) ?: run { onClose(); return }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Scaffold(
                topBar = {
                    TopAppBar(
                        title = {
                            Column {
                                Text(listOf(sub.agent, sub.model).filter { it.isNotBlank() }.joinToString(" · "))
                                Text(listOf(stateLabel(sub.state), sub.progress).filter { it.isNotBlank() }.joinToString(" · "), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            }
                        },
                        navigationIcon = { IconButton(onClick = onClose) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
                        actions = {
                            if (all.size > 1) {
                                IconButton(onClick = { current = all[(index - 1 + all.size) % all.size].id }) { Icon(Icons.AutoMirrored.Filled.KeyboardArrowLeft, null) }
                                Text("${index + 1}/${all.size}", style = MaterialTheme.typography.labelLarge)
                                IconButton(onClick = { current = all[(index + 1) % all.size].id }) { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null) }
                            }
                        },
                    )
                },
            ) { padding ->
                val list = rememberLazyListState()
                LaunchedEffect(current, sub.items.size) { if (sub.items.isNotEmpty()) list.scrollToItem(sub.items.size - 1) }
                LazyColumn(Modifier.fillMaxSize().padding(padding), state = list, contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    itemsIndexed(sub.items, key = { i, _ -> "$current-$i" }) { _, row -> ConversationRow(state, row, actions = false) }
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ConversationRow(state: AppState, row: ItemRec, actions: Boolean = true, onDelegation: (String) -> Unit = {}, onTool: (ItemRec) -> Unit = {}) {
    val po = LocalPo.current
    when (row.kind) {
        ItemKind.USER -> Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            val (text, files) = attached(row.text)
            files.forEach { name -> AttachedChip(name) }
            if (text.isNotBlank()) {
                Surface(color = MaterialTheme.colorScheme.primaryContainer, shape = RoundedCornerShape(18.dp, 18.dp, 4.dp, 18.dp)) {
                    SelectionContainer { Text(rich(text, po.link), Modifier.padding(horizontal = 14.dp, vertical = 10.dp).widthIn(max = 320.dp), style = MaterialTheme.typography.bodyLarge) }
                }
            }
        }
        ItemKind.ASSISTANT -> Column(Modifier.fillMaxWidth()) {
            SelectionContainer { RichText(row.text + if (row.streaming) " ▍" else "") }
            if (actions && !row.streaming && row.text.isNotBlank()) AnswerActions(state, row.text)
        }
        ItemKind.THINKING -> WorkBlock(
            mark = { if (row.streaming) CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp) else Text("✻", color = MaterialTheme.colorScheme.onSurfaceVariant) },
            title = stringResource(R.string.reasoning, (row.chars / 4u).toInt()),
            summary = "",
            body = row.text,
            italic = true,
        )
        ItemKind.TOOL -> {
            val (mark, color) = when (row.status) {
                ToolStatus.ERROR -> "✕" to MaterialTheme.colorScheme.error
                ToolStatus.DENIED -> "⊘" to MaterialTheme.colorScheme.error
                else -> "✓" to po.ok
            }
            WorkBlock(
                mark = { if (row.status == ToolStatus.RUNNING) CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp) else Text(mark, color = color, fontWeight = FontWeight.Bold) },
                title = row.title,
                // The summary often repeats the tool's name: said once.
                summary = row.text.removePrefix(row.title).trimStart(),
                body = row.text,
                diff = row.detail,
                mono = true,
                onOpen = { onTool(row) },
            )
        }
        ItemKind.ASKED -> Surface(shape = RoundedCornerShape(12.dp), color = po.work) {
            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                Text(row.text, style = MaterialTheme.typography.bodyMedium)
                Text("→ ${row.detail}", fontWeight = FontWeight.SemiBold)
            }
        }
        ItemKind.DELEGATION -> WorkBlock(
            mark = { Icon(PoIcons.Agents, null, Modifier.size(15.dp), tint = MaterialTheme.colorScheme.primary) },
            title = row.title + if (row.streaming) " · " + stringResource(R.string.background) else "",
            summary = row.detail.lineSequence().firstOrNull().orEmpty(),
            body = row.detail,
            extra = if (actions) ({ TextButton(onClick = { onDelegation(row.text) }) { Text(stringResource(R.string.follow_subagent)) } }) else null,
        )
        ItemKind.NOTICE -> Text(row.text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, fontStyle = FontStyle.Italic)
        ItemKind.WARNING -> Text(row.text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error)
        ItemKind.OUTPUT -> Surface(shape = RoundedCornerShape(14.dp), color = po.work, modifier = Modifier.fillMaxWidth().border(1.dp, po.workBorder, RoundedCornerShape(14.dp))) {
            SelectionContainer {
                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    Text(row.title, style = Mono, fontWeight = FontWeight.SemiBold, color = MaterialTheme.colorScheme.primary)
                    row.lines.forEach { l ->
                        when (l.kind) {
                            "section" -> Text(l.text, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(top = 4.dp))
                            "warn" -> Text(rich(l.text, po.link), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
                            "dim" -> Text(rich(l.text, po.link), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            "item" -> Text(buildAnnotatedString { withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(l.text) }; if (l.detail.isNotBlank()) { append(" — "); append(rich(l.detail, po.link)) } }, style = MaterialTheme.typography.bodyMedium)
                            "field" -> Text(buildAnnotatedString { append("${l.text} : "); withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(rich(l.detail, po.link)) } }, style = MaterialTheme.typography.bodyMedium)
                            else -> Text(rich(l.text, po.link), style = MaterialTheme.typography.bodyMedium)
                        }
                    }
                }
            }
        }
        ItemKind.SUMMARY -> Text(row.text, Modifier.fillMaxWidth().padding(vertical = 2.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

private fun copy(context: android.content.Context, text: String) {
    context.getSystemService(android.content.ClipboardManager::class.java)?.setPrimaryClip(android.content.ClipData.newPlainText("Po", text))
}

private val UPLOAD = Regex("""@\.po/uploads/(\S+)""")
private val STAMP = Regex("""^\d{8}-\d{6}-""")

/** A message's text, and the files sent with it, by their names (without the folder nor the
 *  time the PC put before them). */
fun attached(text: String): Pair<String, List<String>> {
    val files = UPLOAD.findAll(text).map { it.groupValues[1].replace(STAMP, "") }.toList()
    return UPLOAD.replace(text, "").trim() to files
}

@Composable
private fun AttachedChip(name: String) {
    val image = name.substringAfterLast('.', "").lowercase() in setOf("jpg", "jpeg", "png", "webp", "gif", "heic")
    Surface(shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.secondaryContainer) {
        Row(Modifier.padding(horizontal = 10.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(if (image) PoIcons.Image else PoIcons.File, null, Modifier.size(16.dp))
            Spacer(Modifier.width(6.dp))
            Text(name, style = MaterialTheme.typography.labelLarge, modifier = Modifier.widthIn(max = 280.dp))
        }
    }
}

/** Under an answer: read it aloud, copy it. */
@Composable
private fun AnswerActions(state: AppState, text: String) {
    val context = LocalContext.current
    val copied = stringResource(R.string.copied)
    Row(Modifier.padding(top = 2.dp), verticalAlignment = Alignment.CenterVertically) {
        SmallAction(PoIcons.Copy, stringResource(R.string.copy)) {
            copy(context, text)
        }
        val speaking = state.speaking == text
        SmallAction(if (speaking) PoIcons.Stop else PoIcons.Speak, stringResource(if (speaking) R.string.speak_stop else R.string.speak), active = speaking) { state.speak(text) }
    }
}

@Composable
private fun SmallAction(icon: ImageVector, label: String, active: Boolean = false, onClick: () -> Unit) {
    IconButton(onClick = onClick, modifier = Modifier.size(36.dp)) {
        Icon(icon, label, Modifier.size(18.dp), tint = if (active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/**
 * A block of work (reasoning, a tool, a subagent): set apart on its own background. A touch
 * anywhere on it opens or closes it; a long press copies what it holds.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun WorkBlock(
    mark: @Composable () -> Unit,
    title: String,
    summary: String,
    body: String,
    italic: Boolean = false,
    mono: Boolean = false,
    diff: String = "",
    extra: (@Composable () -> Unit)? = null,
    onOpen: (() -> Unit)? = null,
) {
    val po = LocalPo.current
    var open by rememberSaveable { mutableStateOf(false) }
    val openable = body.isNotBlank() || diff.isNotBlank() || onOpen != null
    val shape = RoundedCornerShape(12.dp)
    Surface(Modifier.fillMaxWidth().clip(shape).border(1.dp, po.workBorder, shape), color = po.work, shape = shape) {
        Column(Modifier.animateContentSize()) {
            Row(
                Modifier.fillMaxWidth().clickable(enabled = openable) { if (onOpen != null) onOpen() else open = !open }.padding(horizontal = 12.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.width(20.dp), contentAlignment = Alignment.Center) { mark() }
                Spacer(Modifier.width(8.dp))
                val folded = summary.isNotBlank() && !open
                Text(title, style = if (mono) Mono.copy(fontWeight = FontWeight.SemiBold) else MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = if (folded) Modifier.widthIn(max = 160.dp) else Modifier.weight(1f))
                if (folded) {
                    Spacer(Modifier.width(8.dp))
                    Text(summary, Modifier.weight(1f), style = if (mono) Mono else MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                if (onOpen != null) Icon(PoIcons.Chevron, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                else if (openable) Icon(Icons.Default.KeyboardArrowDown, null, Modifier.size(20.dp).rotate(if (open) 180f else 0f), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            AnimatedVisibility(open) {
                // A tap folds it; a long press selects words of it.
                Column(
                    Modifier.fillMaxWidth().pointerInput(Unit) { detectTapGestures(onTap = { open = false }) }.padding(start = 12.dp, end = 12.dp, bottom = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    if (body.isNotBlank()) SelectionContainer { Text(body, style = if (mono) Mono else MaterialTheme.typography.bodyMedium, fontStyle = if (italic) FontStyle.Italic else FontStyle.Normal, color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.85f)) }
                    if (diff.isNotBlank()) Diff(diff)
                    extra?.invoke()
                }
            }
        }
    }
}

@Composable
private fun Diff(diff: String) {
    Surface(color = MaterialTheme.colorScheme.background, shape = RoundedCornerShape(8.dp)) {
        Column(Modifier.horizontalScroll(rememberScrollState()).padding(8.dp)) {
            diff.lines().take(400).forEach { line ->
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

/** An answer: code blocks in a monospace box, headings, lists, **bold**, `code` and links. */
@Composable
private fun RichText(text: String) {
    val link = LocalPo.current.link
    val parts = text.split("```")
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        parts.forEachIndexed { i, part ->
            if (i % 2 == 1) {
                val code = part.substringAfter('\n', part)
                Surface(color = LocalPo.current.work, shape = RoundedCornerShape(10.dp)) {
                    Text(code.trimEnd(), Modifier.horizontalScroll(rememberScrollState()).padding(10.dp), style = Mono, softWrap = false)
                }
            } else if (part.isNotBlank()) {
                // Tables apart: drawn as a grid, the rest as text.
                tableRuns(part.trim('\n')).forEach { (table, chunk) ->
                    if (table) Table(chunk) else if (chunk.isNotBlank()) Text(blocks(chunk.trim('\n'), link), style = MaterialTheme.typography.bodyLarge)
                }
            }
        }
    }
}

/** The text cut into runs of table lines (`| a | b |`) and the rest, in order. */
private fun tableRuns(text: String): List<Pair<Boolean, String>> {
    val out = mutableListOf<Pair<Boolean, MutableList<String>>>()
    for (line in text.lines()) {
        val table = line.trim().let { it.startsWith("|") && it.endsWith("|") && it.length > 2 }
        if (out.isEmpty() || out.last().first != table) out.add(table to mutableListOf())
        out.last().second.add(line)
    }
    // A single line between bars is no table.
    return out.map { (table, lines) -> (table && lines.size >= 2) to lines.joinToString("\n") }
}

@Composable
private fun Table(text: String) {
    val link = LocalPo.current.link
    val rows = text.lines()
        .map { line -> line.trim().removePrefix("|").removeSuffix("|").split("|").map { it.trim() } }
        .filterNot { cells -> cells.all { c -> c.isNotEmpty() && c.all { it == '-' || it == ':' || it == ' ' } } }
    val columns = rows.maxOfOrNull { it.size } ?: return
    val shape = RoundedCornerShape(10.dp)
    Surface(Modifier.border(1.dp, LocalPo.current.workBorder, shape), shape = shape, color = Color.Transparent) {
        Column(Modifier.horizontalScroll(rememberScrollState())) {
            rows.forEachIndexed { r, cells ->
                Row(Modifier.background(if (r == 0) LocalPo.current.work else Color.Transparent)) {
                    for (c in 0 until columns) {
                        Text(
                            rich(cells.getOrElse(c) { "" }, link),
                            Modifier.width(150.dp).padding(horizontal = 10.dp, vertical = 7.dp),
                            style = MaterialTheme.typography.bodyMedium,
                            fontWeight = if (r == 0) FontWeight.SemiBold else FontWeight.Normal,
                        )
                    }
                }
                if (r < rows.size - 1) Box(Modifier.width(150.dp * columns).height(1.dp).background(LocalPo.current.workBorder))
            }
        }
    }
}

/** Headings and list marks of an answer's text, its inline marks inside. */
private fun blocks(text: String, link: Color): AnnotatedString = buildAnnotatedString {
    text.lines().forEachIndexed { i, raw ->
        if (i > 0) append("\n")
        val line = raw.trimEnd()
        val hashes = line.takeWhile { it == '#' }.length
        when {
            hashes in 1..6 && line.getOrNull(hashes) == ' ' -> withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(rich(line.drop(hashes + 1), link)) }
            line.trimStart().let { it.startsWith("- ") || it.startsWith("* ") || it.startsWith("+ ") } -> {
                val indent = line.length - line.trimStart().length
                append(" ".repeat(indent) + "•  ")
                append(rich(line.trimStart().drop(2), link))
            }
            else -> append(rich(line, link))
        }
    }
}

private val BARE = Regex("""(https?://[^\s<>"'`]+|www\.[^\s<>"'`]+)""")

/** `**bold**`, `` `code` ``, `[text](https://…)` and addresses written as is (a touch opens them). */
fun rich(text: String, link: Color): AnnotatedString = buildAnnotatedString {
    val style = TextLinkStyles(SpanStyle(color = link, textDecoration = TextDecoration.Underline))
    fun url(raw: String) = if (raw.startsWith("www.")) "https://$raw" else raw
    var i = 0
    val plain = StringBuilder()
    fun flush() {
        if (plain.isNotEmpty()) {
            append(plain.toString())
            plain.clear()
        }
    }
    while (i < text.length) {
        val rest = text.substring(i)
        if (rest.startsWith("[")) {
            val close = rest.indexOf("](")
            val end = if (close > 0) rest.indexOf(')', close + 2) else -1
            if (close > 1 && end > close + 2) {
                val target = rest.substring(close + 2, end).trim()
                if (target.startsWith("http://") || target.startsWith("https://") || target.startsWith("www.")) {
                    flush()
                    withLink(LinkAnnotation.Url(url(target), style)) { append(rest.substring(1, close).replace("**", "").replace("`", "")) }
                    i += end + 1
                    continue
                }
            }
        }
        if ((plain.isEmpty() || plain.last().isWhitespace() || plain.last() == '(') && (rest.startsWith("http://") || rest.startsWith("https://") || rest.startsWith("www."))) {
            val found = BARE.find(rest)
            if (found != null && found.range.first == 0) {
                var address = found.value
                while (address.isNotEmpty() && address.last() in ".,;:!?)]}") {
                    if (address.last() == ')' && address.count { it == '(' } >= address.count { it == ')' }) break
                    address = address.dropLast(1)
                }
                if (address.length > 6) {
                    flush()
                    withLink(LinkAnnotation.Url(url(address), style)) { append(address) }
                    i += address.length
                    continue
                }
            }
        }
        if (rest.startsWith("**")) {
            val end = rest.indexOf("**", 2)
            if (end > 2) {
                flush()
                withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(rich(rest.substring(2, end), link)) }
                i += end + 2
                continue
            }
        }
        // *italic* and _italic_: a single mark, closed on the same line, around a word.
        if ((rest.startsWith("*") || rest.startsWith("_")) && rest.length > 2 && !rest[1].isWhitespace()) {
            val mark = rest[0]
            val end = rest.indexOf(mark, 1)
            val wordStart = plain.isEmpty() || !plain.last().isLetterOrDigit()
            if (end > 1 && wordStart && !rest[end - 1].isWhitespace() && rest.substring(1, end).none { it == '\n' }) {
                flush()
                withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(rich(rest.substring(1, end), link)) }
                i += end + 1
                continue
            }
        }
        if (rest.startsWith("`")) {
            val end = rest.indexOf('`', 1)
            if (end > 1) {
                flush()
                withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = Color(0x22888888))) { append(rest.substring(1, end)) }
                i += end + 1
                continue
            }
        }
        plain.append(text[i])
        i++
    }
    flush()
}
