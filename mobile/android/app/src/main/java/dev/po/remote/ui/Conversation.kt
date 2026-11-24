package dev.po.remote.ui

import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.PlayArrow

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.material.icons.filled.Add
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.ui.input.pointer.PointerEventPass
import kotlinx.coroutines.withTimeoutOrNull
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.BorderStroke
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
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.lazy.items
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
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
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
import androidx.compose.material3.RadioButton
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
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
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
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import dev.po.remote.AppState
import dev.po.remote.OPENING
import dev.po.remote.R
import dev.po.remote.Screen
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import uniffi.po_mobile.InputRec
import uniffi.po_mobile.ItemKind
import uniffi.po_mobile.ItemRec
import uniffi.po_mobile.LinkState
import uniffi.po_mobile.GoalRec
import uniffi.po_mobile.LoadingRec
import uniffi.po_mobile.MenuRec
import uniffi.po_mobile.StatusRec
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
    // Where that call sits in the conversation: the sheet reads the row live from there, so a call
    // opened while it runs shows its end (output, done) instead of a copy frozen at the tap.
    var toolAt by remember { mutableStateOf(-1) }
    val showTool: (ItemRec) -> Unit = { tool -> toolShown = tool; toolAt = state.rows.indexOfFirst { it === tool } }
    var closingWithModel by remember { mutableStateOf<String?>(null) }
    var verbosityOpen by remember { mutableStateOf(false) }
    val readOnly = screen.key.startsWith("saved:")
    val link = state.links[screen.pc]
    // A model loading on the PC: the app counts the seconds itself, so a status that only ages
    // the line does not need to come over the wire. The PC's `sinceMs` is used when the two
    // clocks agree (both are network-synced most of the time); far apart, counting from here
    // reads better than a wrong number.
    val loading = status?.loading
    var loadingSeconds by remember(loading?.sinceMs) { mutableIntStateOf(0) }
    LaunchedEffect(loading?.sinceMs) {
        if (loading == null) return@LaunchedEffect
        var seconds = ((System.currentTimeMillis() - loading.sinceMs.toLong()) / 1000).toInt()
        if (seconds !in 0..3600) seconds = 0
        loadingSeconds = seconds
        while (true) {
            delay(1000)
            seconds += 1
            loadingSeconds = seconds
        }
    }
    val scope = rememberCoroutineScope()
    var userScrolledUp by remember { mutableStateOf(false) }
    var hasScrolledToBottomOnLoad by remember(screen.key) { mutableStateOf(false) }

    val isDragged by list.interactionSource.collectIsDraggedAsState()
    val isAtBottom by remember {
        derivedStateOf {
            val total = list.layoutInfo.totalItemsCount
            if (total <= 1) return@derivedStateOf true
            val lastVisible = list.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            lastVisible >= total - 1
        }
    }

    // Scroll to bottom on initial screen load / session re-entry
    LaunchedEffect(screen.key, state.loading) {
        if (!state.loading) {
            snapshotFlow { list.layoutInfo.totalItemsCount }
                .filter { it > 0 }
                .first()
            if (!hasScrolledToBottomOnLoad) {
                list.scrollToItem(list.layoutInfo.totalItemsCount - 1)
                userScrolledUp = false
                hasScrolledToBottomOnLoad = true
            }
        }
    }
    LaunchedEffect(isDragged, list.isScrollInProgress, isAtBottom) {
        if (isDragged) {
            userScrolledUp = !isAtBottom
        } else if (!list.isScrollInProgress && isAtBottom) {
            userScrolledUp = false
        }
    }
    LaunchedEffect(state.sent) {
        if (state.sent > 0) {
            userScrolledUp = false
            delay(100)
            val total = list.layoutInfo.totalItemsCount
            if (total > 0) list.animateScrollToItem(total - 1)
        }
    }
    LaunchedEffect(state.rows.size, state.rows.lastOrNull()?.text?.length, state.rows.lastOrNull()?.streaming, state.running, state.verbosity) {
        val total = list.layoutInfo.totalItemsCount
        if (total > 0 && !userScrolledUp && hasScrolledToBottomOnLoad) {
            val lastVisible = list.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            val distance = total - 1 - lastVisible
            if (state.rows.lastOrNull()?.streaming == true || distance > 3) {
                list.scrollToItem(total - 1)
            } else {
                list.animateScrollToItem(total - 1)
            }
        }
    }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            Column {
                TopAppBar(
                    title = {
                        Column(Modifier.clickable { state.openHardware(screen.pc) }) {
                            Text(status?.title?.takeIf { it.isNotBlank() } ?: stringResource(if (screen.key.startsWith(OPENING)) R.string.new_session else R.string.untitled), maxLines = 1, overflow = TextOverflow.Ellipsis)
                            val subParts = listOfNotNull(
                                status?.model?.takeIf { it.isNotBlank() },
                                status?.placement?.takeIf { it.isNotBlank() },
                                if (status?.labs == true) "Po Labs" else null,
                            )
                            val sub = subParts.joinToString(" · ")
                            // The model on one line; the branch and its changes are in the strip below.
                            if (sub.isNotEmpty()) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Box(Modifier.size(6.dp).clip(CircleShape).background(PoColors.Moss))
                                    Spacer(Modifier.width(4.dp))
                                    Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                }
                            }
                        }
                    },
                    navigationIcon = { IconButton(onClick = { state.back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
                    actions = {
                        IconButton(onClick = { state.openHardware(screen.pc) }) {
                            Icon(PoIcons.Hardware, stringResource(R.string.hardware_title), Modifier.size(20.dp), tint = MaterialTheme.colorScheme.primary)
                        }
                        if (state.subagents.isNotEmpty()) SubagentsChip(state.subagents) { agentsOpen = true }
                        if (status != null && status.contextWindow > 0u) ContextGauge(status.contextUsed.toLong(), status.contextWindow.toLong()) { contextOpen = true }
                        IconButton(onClick = { overflow = true }) { Icon(Icons.Default.MoreVert, null) }
                        DropdownMenu(overflow, { overflow = false }) {
                        if (readOnly) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.resume_here)) },
                                leadingIcon = { Icon(Icons.Default.PlayArrow, null) },
                                onClick = { overflow = false; state.resumeSaved(screen.pc, screen.key.removePrefix("saved:")) }
                            )
                        } else {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.commands)) },
                                leadingIcon = { Icon(PoIcons.Terminal, null) },
                                onClick = { overflow = false; state.draft = "/" }
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.profiles)) },
                                leadingIcon = { Icon(PoIcons.Brain, null) },
                                onClick = { overflow = false; state.command("/profil") }
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.model_settings)) },
                                leadingIcon = { Icon(PoIcons.Tune, null) },
                                onClick = { overflow = false; state.command("/model settings") }
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.local_models)) },
                                leadingIcon = { Icon(PoIcons.Hardware, null) },
                                onClick = { overflow = false; state.command("/configurer") }
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.close_session)) },
                                leadingIcon = { Icon(Icons.Default.Close, null) },
                                onClick = {
                                    overflow = false
                                    val model = status?.model?.trim().orEmpty()
                                    if (model.isNotEmpty()) {
                                        closingWithModel = model
                                    } else {
                                        state.closeSession()
                                    }
                                }
                            )
                        }
                        DropdownMenuItem(
                            text = { Text("${stringResource(R.string.verbosity)} : ${stringResource(state.verbosity.label)}") },
                            leadingIcon = { Icon(PoIcons.Eye, null) },
                            onClick = { overflow = false; verbosityOpen = true }
                        )
                        HorizontalDivider()
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.app_settings)) },
                            leadingIcon = { Icon(Icons.Default.Settings, null) },
                            onClick = { overflow = false; state.go(Screen.Settings) }
                        )
                    }
                },
            )
            if (status != null) {
                StatusStrip(status, onHardware = { state.openHardware(screen.pc) }, onContext = { contextOpen = true })
            }
            status?.goals?.takeIf { it.isNotEmpty() }?.let { GoalsBar(it) }
            val activeLoading = status?.loading ?: state.globalLoading
            if (activeLoading != null) {
                ModelLoadingBanner(
                    loading = activeLoading,
                    onCancel = {
                        state.unloadModel()
                    },
                    onClick = { state.openModelSheet() }
                )
            }
        }
    },
        bottomBar = {
            Column(Modifier.navigationBarsPadding().imePadding()) {
                if (!state.closed || readOnly) {
                    ContextWarningBanner(state)
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
                Loading(screen, link, status, state)
            } else {
                // Recomputed when the rows change, without copying and comparing the whole list at
                // every frame (a long session holds thousands of rows).
                val blocks by remember { derivedStateOf { groupRows(state.rows, state.verbosity, state.running) } }
                val lastAnswerId by remember { derivedStateOf { blocks.lastOrNull { it is DisplayItem.Single && it.item.kind == ItemKind.ASSISTANT }?.id } }
                LazyColumn(Modifier.fillMaxSize(), state = list, contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                    if (state.rows.isEmpty()) item {
                        if (loading != null) {
                            Box(Modifier.fillMaxWidth().padding(top = 40.dp), contentAlignment = Alignment.Center) {
                                Surface(
                                    shape = RoundedCornerShape(16.dp),
                                    color = MaterialTheme.colorScheme.surfaceVariant,
                                    border = BorderStroke(1.dp, PoColors.Amber.copy(alpha = 0.5f)),
                                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp)
                                ) {
                                    Column(
                                        Modifier.padding(20.dp),
                                        horizontalAlignment = Alignment.CenterHorizontally
                                    ) {
                                        CircularProgressIndicator(Modifier.size(36.dp), strokeWidth = 3.dp, color = PoColors.Amber)
                                        Spacer(Modifier.height(14.dp))
                                        Text(
                                            listOfNotNull(
                                                loading.model.takeIf { it.isNotBlank() },
                                                loading.stage.takeIf { it.isNotBlank() }
                                            ).joinToString(" · ").ifBlank { stringResource(R.string.loading_model) },
                                            style = MaterialTheme.typography.titleMedium,
                                            fontWeight = FontWeight.Bold,
                                            textAlign = TextAlign.Center
                                        )
                                        if (loadingSeconds > 0) {
                                            Spacer(Modifier.height(4.dp))
                                            Text(
                                                stringResource(R.string.loading_seconds, loadingSeconds),
                                                style = MaterialTheme.typography.bodySmall,
                                                color = MaterialTheme.colorScheme.onSurfaceVariant
                                            )
                                        }
                                        Spacer(Modifier.height(16.dp))
                                        Row(
                                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                                            verticalAlignment = Alignment.CenterVertically
                                        ) {
                                            OutlinedButton(
                                                onClick = { state.openModelSheet() },
                                                contentPadding = PaddingValues(horizontal = 14.dp, vertical = 6.dp)
                                            ) {
                                                Icon(PoIcons.Tune, null, Modifier.size(16.dp))
                                                Spacer(Modifier.width(6.dp))
                                                Text(stringResource(R.string.hardware_switch_model), style = MaterialTheme.typography.labelMedium)
                                            }
                                            OutlinedButton(
                                                onClick = {
                                                    state.unloadModel()
                                                },
                                                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                                                border = BorderStroke(1.dp, MaterialTheme.colorScheme.error.copy(alpha = 0.5f)),
                                                contentPadding = PaddingValues(horizontal = 14.dp, vertical = 6.dp)
                                            ) {
                                                Icon(Icons.Default.Close, null, Modifier.size(16.dp))
                                                Spacer(Modifier.width(6.dp))
                                                Text(stringResource(R.string.cancel), style = MaterialTheme.typography.labelMedium)
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            Empty(stringResource(if (state.running) R.string.working else R.string.empty_session))
                        }
                    }
                    items(blocks, key = { it.id }) { block ->
                        when (block) {
                            // While the turn runs, its last answer is an update, not yet the answer.
                            is DisplayItem.Single -> ConversationRow(state, block.item, actions = block.last && !(state.running && block.id == lastAnswerId), onDelegation = { agent ->
                                agentShown = state.subagents.lastOrNull { it.agent == agent }?.id
                                if (agentShown == null && state.subagents.isNotEmpty()) agentsOpen = true
                            }, onTool = showTool)
                            is DisplayItem.GroupedThinking -> GroupedThinkingRow(block)
                            is DisplayItem.GroupedTools -> GroupedToolsRow(block, dynamicAction = state.status?.dynamicAction, onTool = showTool)
                            is DisplayItem.LowWork -> LowWorkRow(block, dynamicAction = state.status?.dynamicAction, onTool = showTool)
                        }
                    }
                    // Working, with nothing being written yet (the model thinks, a tool runs): said
                    // discreetly at the end, where the answer will come. A model still loading on
                    // the PC says what it is doing at the same place - it is waited for before the
                    // turn even starts, so `running` alone would miss it.
                    // At the low level, the live line of activity already says it.
                    if (state.rows.isNotEmpty() && state.rows.lastOrNull()?.streaming != true && (loading != null || state.running)
                        && !(loading == null && (blocks.lastOrNull() as? DisplayItem.LowWork)?.live == true)) {
                        item {
                            if (loading != null) {
                                ModelLoading(
                                    loading,
                                    loadingSeconds,
                                    onCancel = {
                                        state.unloadModel()
                                    },
                                    onOpenModels = { state.openModelSheet() }
                                )
                            } else {
                                Working(state.status?.paused == true)
                            }
                        }
                    }
                    item { Spacer(Modifier.height(8.dp)) }
                }
            }
            if (!state.loading && (link is LinkState.Connecting || link is LinkState.Down)) Reconnecting(Modifier.align(Alignment.TopCenter))
            if (state.menuLoading && state.menu == null) LinearProgressIndicator(Modifier.fillMaxWidth().align(Alignment.TopCenter))

            val showScrollToBottom = !state.loading && hasScrolledToBottomOnLoad && list.layoutInfo.totalItemsCount > 1 && !isAtBottom
            AnimatedVisibility(
                visible = showScrollToBottom,
                enter = fadeIn(tween(150)) + slideInVertically(tween(150)) { it / 2 },
                exit = fadeOut(tween(150)) + slideOutVertically(tween(150)) { it / 2 },
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 12.dp)
            ) {
                Surface(
                    onClick = {
                        userScrolledUp = false
                        scope.launch {
                            val total = list.layoutInfo.totalItemsCount
                            if (total > 0) list.animateScrollToItem(total - 1)
                        }
                    },
                    shape = CircleShape,
                    color = MaterialTheme.colorScheme.surfaceContainerHigh,
                    contentColor = MaterialTheme.colorScheme.primary,
                    shadowElevation = 4.dp,
                    border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f)),
                    modifier = Modifier.size(42.dp)
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Icon(
                            imageVector = Icons.Default.KeyboardArrowDown,
                            contentDescription = stringResource(R.string.scroll_to_bottom),
                            tint = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.size(26.dp)
                        )
                    }
                }
            }
        }
    }
    state.approval?.let { approval ->
        val approvalScroll = rememberScrollState()
        ModalBottomSheet(onDismissRequest = {}) {
            Column(Modifier.padding(20.dp).navigationBarsPadding()) {
                Text(stringResource(R.string.approval_title), style = MaterialTheme.typography.titleLarge)
                Spacer(Modifier.height(8.dp))
                Text(approval.tool, fontWeight = FontWeight.SemiBold, color = MaterialTheme.colorScheme.primary)
                Surface(Modifier.fillMaxWidth().padding(vertical = 10.dp).heightIn(max = 240.dp).verticalScroll(approvalScroll), color = LocalPo.current.work, shape = RoundedCornerShape(10.dp)) {
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
    toolShown?.let { shown ->
        val live = state.rows.getOrNull(toolAt)?.takeIf { it.kind == ItemKind.TOOL && it.title == shown.title } ?: shown
        ToolSheet(live) { toolShown = null; toolAt = -1 }
    }
    if (agentsOpen) SubagentsSheet(state.subagents, onClose = { agentsOpen = false }) { agentsOpen = false; agentShown = it }
    agentShown?.let { id -> SubagentView(state, id) { agentShown = null } }
    // The level is chosen, not cycled: each one with what it shows, the current one marked.
    if (verbosityOpen) {
        AlertDialog(
            onDismissRequest = { verbosityOpen = false },
            icon = { Icon(PoIcons.Eye, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
            title = { Text(stringResource(R.string.verbosity)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Verbosity.entries.forEach { v ->
                        val on = state.verbosity == v
                        Row(
                            Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp))
                                .clickable { state.chooseVerbosity(v); verbosityOpen = false }
                                .padding(vertical = 8.dp, horizontal = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(selected = on, onClick = { state.chooseVerbosity(v); verbosityOpen = false })
                            Spacer(Modifier.width(8.dp))
                            Column {
                                Text(stringResource(v.label), fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal)
                                Text(stringResource(v.hint), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                    }
                }
            },
            confirmButton = { TextButton(onClick = { verbosityOpen = false }) { Text(stringResource(R.string.close)) } },
        )
    }
    closingWithModel?.let { modelName ->
        AlertDialog(
            onDismissRequest = { closingWithModel = null },
            icon = { Icon(PoIcons.Hardware, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
            title = { Text(stringResource(R.string.exit_unload_title)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(stringResource(R.string.exit_unload_message, modelName))
                    Text(
                        stringResource(R.string.exit_unload_hint),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        closingWithModel = null
                        state.closeSession(unloadModel = true)
                    }
                ) {
                    Text(stringResource(R.string.exit_unload_action))
                }
            },
            dismissButton = {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(
                        onClick = {
                            closingWithModel = null
                        }
                    ) {
                        Text(stringResource(R.string.cancel))
                    }
                    TextButton(
                        onClick = {
                            closingWithModel = null
                            state.closeSession(unloadModel = false)
                        }
                    ) {
                        Text(stringResource(R.string.exit_keep_action))
                    }
                }
            }
        )
    }
}

/** Loading the conversation and auto-loading model with clear visibility. */
@Composable
private fun Loading(screen: Screen.Conversation, link: LinkState?, status: StatusRec?, state: AppState) {
    var seconds by remember { mutableIntStateOf(0) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1000)
            seconds += 1
        }
    }
    val loading = status?.loading
    val activeModel = loading?.model ?: status?.model?.takeIf { it.isNotBlank() }

    Column(
        Modifier
            .fillMaxSize()
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center
    ) {
        if (activeModel != null) {
            Surface(
                shape = RoundedCornerShape(20.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp)
            ) {
                Column(
                    Modifier.padding(20.dp),
                    horizontalAlignment = Alignment.CenterHorizontally
                ) {
                    CircularProgressIndicator(Modifier.size(44.dp), strokeWidth = 3.dp, color = PoColors.Amber)
                    Spacer(Modifier.height(16.dp))
                    Text(
                        stringResource(R.string.loading_model),
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.Bold
                    )
                    Spacer(Modifier.height(8.dp))
                    Surface(
                        shape = CircleShape,
                        color = MaterialTheme.colorScheme.primaryContainer,
                    ) {
                        Text(
                            activeModel,
                            Modifier.padding(horizontal = 14.dp, vertical = 5.dp),
                            style = MaterialTheme.typography.labelLarge,
                            fontWeight = FontWeight.SemiBold,
                            color = MaterialTheme.colorScheme.onPrimaryContainer
                        )
                    }
                    if (status?.placement != null) {
                        Spacer(Modifier.height(8.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Box(Modifier.size(8.dp).clip(CircleShape).background(PoColors.Moss))
                            Spacer(Modifier.width(6.dp))
                            Text(
                                status.placement!!,
                                style = MaterialTheme.typography.bodySmall,
                                fontWeight = FontWeight.Medium,
                                color = PoColors.Moss
                            )
                        }
                    }
                    val step = if (loading != null) loadingText(loading) else stringResource(R.string.loading_starting)
                    Spacer(Modifier.height(12.dp))
                    Text(
                        step,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        textAlign = TextAlign.Center
                    )
                    Spacer(Modifier.height(6.dp))
                    Text(
                        stringResource(R.string.loading_seconds, seconds),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                    Spacer(Modifier.height(16.dp))
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        OutlinedButton(
                            onClick = { state.openModelSheet() },
                            contentPadding = PaddingValues(horizontal = 14.dp, vertical = 6.dp)
                        ) {
                            Icon(PoIcons.Tune, null, Modifier.size(16.dp))
                            Spacer(Modifier.width(6.dp))
                            Text(stringResource(R.string.hardware_switch_model), style = MaterialTheme.typography.labelMedium)
                        }
                        OutlinedButton(
                            onClick = {
                                state.unloadModel()
                            },
                            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                            border = BorderStroke(1.dp, MaterialTheme.colorScheme.error.copy(alpha = 0.5f)),
                            contentPadding = PaddingValues(horizontal = 14.dp, vertical = 6.dp)
                        ) {
                            Icon(Icons.Default.Close, null, Modifier.size(16.dp))
                            Spacer(Modifier.width(6.dp))
                            Text(stringResource(R.string.cancel), style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
            }
        } else {
            val stage = when {
                screen.key.startsWith(OPENING) -> stringResource(R.string.loading_starting)
                link is LinkState.Up -> stringResource(R.string.loading_conversation)
                link is LinkState.Down -> stringResource(R.string.loading_retry)
                else -> stringResource(R.string.loading_connecting)
            }
            CircularProgressIndicator(Modifier.size(42.dp), strokeWidth = 3.dp)
            Spacer(Modifier.height(16.dp))
            Text(stage, style = MaterialTheme.typography.bodyLarge)
            if (seconds > 0) Text(stringResource(R.string.loading_seconds, seconds), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun Working(paused: Boolean) {
    Row(Modifier.padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        if (paused) Icon(PoIcons.Pause, null, Modifier.size(14.dp), tint = LocalPo.current.warn)
        else WorkingDots(PoColors.Amber)
        Spacer(Modifier.width(10.dp))
        Text(stringResource(if (paused) R.string.pausing else R.string.working), style = MaterialTheme.typography.bodyMedium, color = if (paused) MaterialTheme.colorScheme.onSurfaceVariant else PoColors.Amber)
    }
}

/** A model is loading on the PC: what it is doing, and for how long. */
@Composable
private fun ModelLoading(
    loading: LoadingRec,
    seconds: Int,
    onCancel: () -> Unit,
    onOpenModels: () -> Unit,
    modifier: Modifier = Modifier
) {
    Surface(
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.7f),
        border = BorderStroke(1.dp, PoColors.Amber.copy(alpha = 0.4f)),
        modifier = modifier.fillMaxWidth().padding(vertical = 4.dp)
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp, color = PoColors.Amber)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    listOfNotNull(
                        loading.model.takeIf { it.isNotBlank() },
                        loading.stage.takeIf { it.isNotBlank() },
                        if (seconds > 0) stringResource(R.string.loading_seconds, seconds) else null,
                    ).joinToString(" · "),
                    style = MaterialTheme.typography.bodySmall,
                    color = LocalPo.current.warn,
                    fontWeight = FontWeight.Medium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(Modifier.width(8.dp))
            IconButton(
                onClick = onOpenModels,
                modifier = Modifier.size(28.dp)
            ) {
                Icon(PoIcons.Tune, null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.primary)
            }
            IconButton(
                onClick = onCancel,
                modifier = Modifier.size(28.dp)
            ) {
                Icon(Icons.Default.Close, stringResource(R.string.cancel), Modifier.size(16.dp), tint = MaterialTheme.colorScheme.error)
            }
        }
    }
}

/** The same words, for where a spinner already stands (a waiting screen's text). */
@Composable
private fun loadingText(loading: LoadingRec): String =
    listOfNotNull(
        stringResource(R.string.loading_model),
        loading.model.takeIf { it.isNotBlank() },
        loading.stage.takeIf { it.isNotBlank() },
    ).joinToString(" · ")

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
    Surface(Modifier.fillMaxWidth().padding(horizontal = 10.dp).heightIn(max = 140.dp), shape = RoundedCornerShape(16.dp), color = MaterialTheme.colorScheme.surfaceVariant, shadowElevation = 2.dp) {
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
    val focus = LocalFocusManager.current
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
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Sentences,
                    imeAction = ImeAction.Send,
                ),
                keyboardActions = KeyboardActions(
                    onSend = {
                        if (hasText) {
                            focus.clearFocus()
                            state.submit()
                        }
                    }
                ),
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
                    // Sent: the keyboard closes, as in the Claude app, so the work can be read; it no longer
                    // reopens on its own when the turn ends (the field kept the focus).
                    onClick = { if (stop) state.stop() else { focus.clearFocus(); state.submit() } },
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

/** Plan, builder, bypass: modern, ergonomic bottom sheet with generous touch targets. */
@Composable
private fun ModeChip(state: AppState, mode: String) {
    var open by remember { mutableStateOf(false) }
    val (label, icon, color) = when (mode) {
        "auto" -> Triple(stringResource(R.string.mode_auto), PoIcons.Auto, Color(0xFF9D7CD8))
        "auto · bypass" -> Triple(stringResource(R.string.mode_auto_bypass), PoIcons.Auto, MaterialTheme.colorScheme.error)
        "plan" -> Triple(stringResource(R.string.mode_plan), PoIcons.Plan, Color(0xFF4F8EF7))
        "bypass" -> Triple(stringResource(R.string.mode_bypass), PoIcons.Bypass, MaterialTheme.colorScheme.error)
        "builder" -> Triple(stringResource(R.string.mode_build), PoIcons.Builder, LocalPo.current.ok)
        else -> Triple(stringResource(R.string.mode_build), PoIcons.Builder, LocalPo.current.ok)
    }
    Surface(
        modifier = Modifier
            .height(42.dp)
            .clip(RoundedCornerShape(21.dp))
            .clickable { open = true },
        shape = RoundedCornerShape(21.dp),
        color = color.copy(alpha = 0.15f),
        border = BorderStroke(1.5.dp, color.copy(alpha = 0.45f))
    ) {
        Row(
            Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Icon(icon, null, Modifier.size(16.dp), tint = color)
            Spacer(Modifier.width(6.dp))
            Text(label, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold, color = color)
            Spacer(Modifier.width(2.dp))
            Icon(Icons.Default.KeyboardArrowDown, null, Modifier.size(16.dp), tint = color)
        }
    }
    if (open) {
        WorkModeSheet(state, mode) { open = false }
    }
}

@Composable
private fun ModelChip(state: AppState, pinned: String?, model: String, models: List<String>, modifier: Modifier = Modifier) {
    var open by remember { mutableStateOf(false) }
    val status = state.status
    val modelLabel = pinned ?: model.ifBlank { stringResource(R.string.model_auto) }
    Surface(
        modifier = modifier
            .padding(start = 6.dp)
            .height(42.dp)
            .clip(RoundedCornerShape(21.dp))
            .clickable { open = true },
        shape = RoundedCornerShape(21.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant)
    ) {
        Row(
            Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Icon(PoIcons.Hardware, null, Modifier.size(14.dp), tint = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.width(6.dp))
            Text(
                modelLabel,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.Medium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
            if (status?.effort != null) {
                Spacer(Modifier.width(4.dp))
                Text(
                    "· ${status.effort}",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                    fontWeight = FontWeight.SemiBold
                )
            }
            Spacer(Modifier.width(2.dp))
            Icon(Icons.Default.KeyboardArrowDown, null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
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
            LazyColumn(Modifier.heightIn(max = 520.dp), contentPadding = PaddingValues(vertical = 6.dp)) {
                menu.note?.let { note ->
                    item {
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
                }
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
                    // The agent and its state, its model in small, then what it does: one thing per
                    // line, nothing squeezed against the edge.
                    Row(Modifier.padding(horizontal = 14.dp, vertical = 12.dp), verticalAlignment = Alignment.Top) {
                        Box(Modifier.padding(top = 3.dp)) { SubagentMark(s.state) }
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(s.agent, Modifier.weight(1f), fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                Spacer(Modifier.width(8.dp))
                                Text(stateLabel(s.state), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                            }
                            if (s.model.isNotBlank()) Text(s.model, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Text(s.progress.ifBlank { s.task.lineSequence().firstOrNull().orEmpty() }, style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        }
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
/**
 * A subagent at a glance, as Claude Code shows its agents: its mission, what it says it is doing,
 * one line per tool call (the last ones; the rest counted), then its result. Its whole
 * conversation stays one touch away.
 */
@Composable
private fun SubagentSummary(sub: SubagentRec, modifier: Modifier, onTool: (ItemRec) -> Unit, onFull: () -> Unit) {
    val tools = sub.items.filter { it.kind == ItemKind.TOOL }
    val result = if (sub.state == "done" || sub.state == "failed") sub.items.lastOrNull { it.kind == ItemKind.ASSISTANT && it.text.isNotBlank() } else null
    val running = sub.state == "running"
    var missionOpen by remember(sub.id) { mutableStateOf(false) }
    var allTools by remember(sub.id) { mutableStateOf(false) }
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    LazyColumn(modifier.fillMaxSize(), contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(bottom = 8.dp)) {
                when (sub.state) {
                    "running" -> WorkingDots(PoColors.Amber)
                    "failed" -> Text("✕", color = MaterialTheme.colorScheme.error, fontWeight = FontWeight.Bold)
                    "paused" -> Icon(PoIcons.Pause, null, Modifier.size(14.dp), tint = LocalPo.current.warn)
                    else -> Text("✓", color = LocalPo.current.ok, fontWeight = FontWeight.Bold)
                }
                Spacer(Modifier.width(8.dp))
                Text(
                    listOf(stateLabel(sub.state), stringResource(R.string.subagent_tools, tools.size)).joinToString(" · "),
                    style = MaterialTheme.typography.labelLarge,
                    color = if (running) PoColors.Amber else muted,
                )
            }
        }
        if (sub.task.isNotBlank()) {
            item { SectionLabel(stringResource(R.string.subagent_mission)) }
            item {
                Text(
                    sub.task.trim(),
                    Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).clickable { missionOpen = !missionOpen }.animateContentSize(),
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = if (missionOpen) Int.MAX_VALUE else 3,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (running && sub.progress.isNotBlank()) {
            item { SectionLabel(stringResource(R.string.subagent_now)) }
            item { Text(sub.progress, style = MaterialTheme.typography.bodyMedium, fontStyle = FontStyle.Italic, color = PoColors.Amber) }
        }
        if (tools.isNotEmpty()) {
            item { SectionLabel(stringResource(R.string.subagent_actions)) }
            val hidden = if (allTools) 0 else (tools.size - 6).coerceAtLeast(0)
            if (hidden > 0) {
                item {
                    Text(
                        stringResource(R.string.subagent_more_tools, hidden),
                        Modifier.clip(RoundedCornerShape(8.dp)).clickable { allTools = true }.padding(vertical = 6.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = muted,
                    )
                }
            }
            items(tools.drop(hidden).size) { i ->
                val tool = tools.drop(hidden)[i]
                WorkLine(
                    text = verb(tool),
                    mono = target(tool).ifBlank { null },
                    running = tool.status == ToolStatus.RUNNING,
                    open = false,
                    failed = if (tool.status == ToolStatus.ERROR || tool.status == ToolStatus.DENIED) 1 else 0,
                    counts = diffCounts(tool.detail),
                    onClick = { onTool(tool) },
                )
            }
        }
        if (result != null) {
            item { SectionLabel(stringResource(R.string.subagent_result)) }
            item { SelectionContainer { RichText(result.text) } }
        }
        item {
            TextButton(onClick = onFull, modifier = Modifier.padding(top = 8.dp)) { Text(stringResource(R.string.subagent_full)) }
        }
    }
}

@Composable
private fun SectionLabel(text: String) {
    Text(text.uppercase(), Modifier.padding(top = 12.dp, bottom = 2.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, letterSpacing = 0.8.sp)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SubagentView(state: AppState, id: String, onClose: () -> Unit) {
    var current by remember(id) { mutableStateOf(id) }
    var toolShown by remember { mutableStateOf<ItemRec?>(null) }
    var closingWithModel by remember { mutableStateOf<String?>(null) }
    val all = state.subagents
    val index = all.indexOfFirst { it.id == current }
    val sub = all.getOrNull(index) ?: run { onClose(); return }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Scaffold(
                topBar = {
                    TopAppBar(
                        title = {
                            // The agent, then its model in small: on one line each, never cut over three.
                            Column {
                                Text(sub.agent, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                if (sub.model.isNotBlank()) Text(sub.model, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
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
                var full by remember(current) { mutableStateOf(false) }
                if (full) {
                    val list = rememberLazyListState()
                    val blocks = remember(sub.items.toList(), state.verbosity) { groupRows(sub.items, state.verbosity) }
                    LaunchedEffect(current, blocks.size) { if (blocks.isNotEmpty()) list.scrollToItem(blocks.size - 1) }
                    LazyColumn(Modifier.fillMaxSize().padding(padding), state = list, contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        items(blocks, key = { "$current-${it.id}" }) { block ->
                            when (block) {
                                is DisplayItem.Single -> ConversationRow(state, block.item, actions = false, onTool = { toolShown = it })
                                is DisplayItem.GroupedThinking -> GroupedThinkingRow(block)
                                is DisplayItem.GroupedTools -> GroupedToolsRow(block, dynamicAction = null, onTool = { toolShown = it })
                                is DisplayItem.LowWork -> LowWorkRow(block, dynamicAction = null, onTool = { toolShown = it })
                            }
                        }
                    }
                } else {
                    SubagentSummary(sub, Modifier.padding(padding), onTool = { toolShown = it }, onFull = { full = true })
                }
            }
        }
    }
    toolShown?.let { ToolSheet(it) { toolShown = null } }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ConversationRow(state: AppState, row: ItemRec, actions: Boolean = true, onDelegation: (String) -> Unit = {}, onTool: (ItemRec) -> Unit = {}) {
    val po = LocalPo.current
    when (row.kind) {
        ItemKind.USER -> Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            val (text, files) = attached(row.text)
            // An image sent is shown by the row that follows, as a thumbnail.
            files.filterNot { it.substringAfterLast('.', "").lowercase() in setOf("jpg", "jpeg", "png", "webp", "gif", "bmp") }.forEach { name -> AttachedChip(name) }
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
        ItemKind.THINKING -> {
            val tokens = (row.chars / 4u).toInt()
            val title = if (row.streaming) stringResource(R.string.thinking_in_progress) else stringResource(R.string.thinking_finished, tokens)
            WorkBlock(
                mark = { if (row.streaming) CircularProgressIndicator(Modifier.size(13.dp), strokeWidth = 1.5.dp, color = PoColors.Amber) else Text("✻", color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f)) },
                title = title,
                summary = "",
                body = row.text,
                italic = true,
                streaming = row.streaming,
                isThinking = true,
                initialOpen = state.verbosity == Verbosity.High,
            )
        }
        ItemKind.TOOL -> {
            val (mark, color) = when (row.status) {
                ToolStatus.ERROR -> "✕" to MaterialTheme.colorScheme.error
                ToolStatus.DENIED -> "⊘" to MaterialTheme.colorScheme.error
                else -> "✓" to po.ok
            }
            val title = if (row.status == ToolStatus.RUNNING) stringResource(R.string.tool_running_one, row.title) else row.title
            val toolTokens = (row.chars / 4u).toInt()
            val rawSummary = row.text.removePrefix(row.title).trimStart()
            val summary = if (toolTokens > 0) {
                if (rawSummary.isNotBlank()) "$rawSummary · ${tokens(toolTokens.toLong())}" else "${tokens(toolTokens.toLong())} tokens"
            } else {
                rawSummary
            }
            WorkBlock(
                mark = { if (row.status == ToolStatus.RUNNING) CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp) else Text(mark, color = color, fontWeight = FontWeight.Bold) },
                title = title,
                summary = summary,
                body = row.text,
                diff = row.detail,
                mono = true,
                onOpen = { onTool(row) },
                initialOpen = state.verbosity == Verbosity.High,
            )
        }
        ItemKind.ASKED -> Surface(shape = RoundedCornerShape(12.dp), color = po.work) {
            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                Text(row.text, style = MaterialTheme.typography.bodyMedium)
                Text("→ ${row.detail}", fontWeight = FontWeight.SemiBold)
            }
        }
        // A subagent: a line like the other work ("explorer · its task"); a touch follows it.
        ItemKind.DELEGATION -> Column(Modifier.fillMaxWidth()) {
            val live = state.subagents.lastOrNull { it.agent == row.text }
            val toolCount = live?.items?.count { it.kind == ItemKind.TOOL } ?: 0
            val busy = live?.state == "running"
            WorkLine(
                text = listOfNotNull(
                    row.text.ifBlank { row.title },
                    if (row.streaming) stringResource(R.string.background) else null,
                    if (toolCount > 0) stringResource(R.string.subagent_tools, toolCount) else null,
                ).joinToString(" · "),
                running = busy,
                open = false,
                icon = { Icon(PoIcons.Agents, null, Modifier.size(15.dp), tint = MaterialTheme.colorScheme.primary) },
                onClick = { onDelegation(row.text) },
            )
            // Working: what it says it does; otherwise its mission's first line.
            val goal = live?.progress?.takeIf { busy && it.isNotBlank() } ?: row.detail.lineSequence().firstOrNull { it.isNotBlank() }.orEmpty()
            if (goal.isNotBlank()) Text(goal, Modifier.padding(start = 23.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
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
                            "item" -> Text(buildAnnotatedString { withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(l.text) }; if (l.detail.isNotBlank()) { append(" - "); append(rich(l.detail, po.link)) } }, style = MaterialTheme.typography.bodyMedium)
                            "field" -> Text(buildAnnotatedString { append("${l.text} : "); withStyle(SpanStyle(fontWeight = FontWeight.Medium)) { append(rich(l.detail, po.link)) } }, style = MaterialTheme.typography.bodyMedium)
                            else -> Text(rich(l.text, po.link), style = MaterialTheme.typography.bodyMedium)
                        }
                    }
                }
            }
        }
        ItemKind.SUMMARY -> Text(row.text, Modifier.fillMaxWidth().padding(vertical = 2.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        ItemKind.IMAGE -> ImageRow(state, row.text)
    }
}

/** An image of the conversation: a thumbnail, full screen on a tap (pinch to zoom, double tap
 *  to zoom in or back). */
@Composable
private fun ImageRow(state: AppState, path: String) {
    var bitmap by remember(path) { mutableStateOf<android.graphics.Bitmap?>(null) }
    var full by remember(path) { mutableStateOf(false) }
    LaunchedEffect(path) { state.image(path) { bitmap = it } }
    val name = path.substringAfterLast('/').substringAfterLast('\\').replace(STAMP, "")
    val shown = bitmap
    if (shown == null) {
        AttachedChip(name)
        return
    }
    androidx.compose.foundation.Image(
        shown.asImageBitmap(),
        contentDescription = name,
        modifier = Modifier
            .widthIn(max = 260.dp)
            .heightIn(max = 260.dp)
            .clip(RoundedCornerShape(12.dp))
            .clickable { full = true },
        contentScale = androidx.compose.ui.layout.ContentScale.Fit,
    )
    if (full) ImageViewer(shown, name) { full = false }
}

@Composable
private fun ImageViewer(bitmap: android.graphics.Bitmap, name: String, onClose: () -> Unit) {
    var scale by remember { mutableStateOf(1f) }
    var offset by remember { mutableStateOf(androidx.compose.ui.geometry.Offset.Zero) }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(
            Modifier
                .fillMaxSize()
                .background(Color.Black)
                .pointerInput(Unit) {
                    detectTransformGestures { _, pan, zoom, _ ->
                        scale = (scale * zoom).coerceIn(1f, 8f)
                        offset = if (scale == 1f) androidx.compose.ui.geometry.Offset.Zero else offset + pan
                    }
                }
                .pointerInput(Unit) {
                    detectTapGestures(onDoubleTap = {
                        if (scale > 1f) {
                            scale = 1f
                            offset = androidx.compose.ui.geometry.Offset.Zero
                        } else {
                            scale = 2.5f
                        }
                    })
                }
        ) {
            androidx.compose.foundation.Image(
                bitmap.asImageBitmap(),
                contentDescription = name,
                modifier = Modifier.fillMaxSize().graphicsLayer(scaleX = scale, scaleY = scale, translationX = offset.x, translationY = offset.y),
                contentScale = androidx.compose.ui.layout.ContentScale.Fit,
            )
            IconButton(onClick = onClose, modifier = Modifier.align(Alignment.TopEnd).padding(8.dp)) {
                Icon(Icons.Default.Close, stringResource(R.string.close), tint = Color.White)
            }
        }
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
    streaming: Boolean = false,
    initialOpen: Boolean = false,
    isThinking: Boolean = false,
) {
    val po = LocalPo.current
    var open by rememberSaveable(initialOpen) { mutableStateOf(initialOpen) }
    val openable = body.isNotBlank() || diff.isNotBlank() || onOpen != null || streaming
    val shape = RoundedCornerShape(12.dp)
    val bgColor = if (isThinking) MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.22f) else po.work.copy(alpha = 0.45f)
    val borderColor = if (isThinking) MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.30f) else po.workBorder.copy(alpha = 0.50f)
    Surface(Modifier.fillMaxWidth().clip(shape).border(if (isThinking) 0.8.dp else 1.dp, borderColor, shape), color = bgColor, shape = shape) {
        Column {
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
            AnimatedVisibility(
                visible = open,
                enter = expandVertically(animationSpec = tween(durationMillis = 180, easing = FastOutSlowInEasing)) + fadeIn(animationSpec = tween(140)),
                exit = shrinkVertically(animationSpec = tween(durationMillis = 180, easing = FastOutSlowInEasing)) + fadeOut(animationSpec = tween(140)),
            ) {
                // A tap folds it; a long press selects words of it.
                Column(
                    Modifier.fillMaxWidth().foldOnTap { open = false }.padding(start = 12.dp, end = 12.dp, bottom = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    if (body.isNotBlank() || streaming) {
                        SelectionContainer {
                            Text(
                                if (body.isNotBlank()) (body + if (streaming) " ▍" else "") else stringResource(R.string.thinking_in_progress),
                                style = if (mono) Mono else MaterialTheme.typography.bodyMedium,
                                fontStyle = if (italic) FontStyle.Italic else FontStyle.Normal,
                                color = MaterialTheme.colorScheme.onSurface.copy(alpha = if (body.isNotBlank()) 0.85f else 0.5f),
                            )
                        }
                    }
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
    // Each line is laid out once and kept: while an answer is written, only its last line
    // changes, and Compose skips the others (the whole answer used to be parsed and measured
    // again at every fragment, twenty times a second).
    val cache = remember(link) { HashMap<String, AnnotatedString>() }
    val pieces = remember(text, link) { richPieces(text, link, cache) }
    Column(Modifier.fillMaxWidth()) {
        pieces.forEachIndexed { i, piece ->
            val gap = if (i == 0) 0.dp else when (piece) {
                is RichPiece.Line -> if (pieces[i - 1] is RichPiece.Line) 0.dp else 6.dp
                else -> 6.dp
            }
            if (gap > 0.dp) Spacer(Modifier.height(gap))
            when (piece) {
                is RichPiece.Code -> Surface(color = LocalPo.current.work, shape = RoundedCornerShape(10.dp)) {
                    Text(piece.code, Modifier.horizontalScroll(rememberScrollState()).padding(10.dp), style = Mono, softWrap = false)
                }
                is RichPiece.Grid -> Table(piece.text)
                is RichPiece.Gap -> Spacer(Modifier.height(10.dp))
                is RichPiece.Line -> Text(piece.text, style = MaterialTheme.typography.bodyLarge)
            }
        }
    }
}

private sealed interface RichPiece {
    data class Code(val code: String) : RichPiece
    data class Grid(val text: String) : RichPiece
    data class Line(val text: AnnotatedString) : RichPiece
    data object Gap : RichPiece
}

/** Code blocks, tables and the lines of prose of an answer, in order. */
private fun richPieces(text: String, link: Color, cache: HashMap<String, AnnotatedString>): List<RichPiece> {
    val out = mutableListOf<RichPiece>()
    // A cache that only grows would keep every line of every answer: it is cleared when large.
    if (cache.size > 4000) cache.clear()
    text.split("```").forEachIndexed { i, part ->
        if (i % 2 == 1) {
            out.add(RichPiece.Code(part.substringAfter('\n', part).trimEnd()))
        } else if (part.isNotBlank()) {
            tableRuns(part.trim('\n')).forEach { (table, chunk) ->
                if (table) {
                    out.add(RichPiece.Grid(chunk))
                } else if (chunk.isNotBlank()) {
                    var blank = false
                    for (line in chunk.trim('\n').lines()) {
                        if (line.isBlank()) {
                            if (!blank) out.add(RichPiece.Gap)
                            blank = true
                            continue
                        }
                        blank = false
                        out.add(RichPiece.Line(cache.getOrPut(line) { blocks(line, link) }))
                    }
                }
            }
        }
    }
    return out
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
    // Walks the text by positions: no copy of the rest at each character (it was quadratic, and a
    // long answer being written was laid out again twenty times a second).
    val style = TextLinkStyles(SpanStyle(color = link, textDecoration = TextDecoration.Underline))
    fun url(raw: String) = if (raw.startsWith("www.")) "https://$raw" else raw
    var i = 0
    var plainStart = 0
    var lastPlain: Char? = null
    fun flush(upTo: Int) {
        if (upTo > plainStart) append(text, plainStart, upTo)
    }
    fun skipTo(next: Int) {
        i = next
        plainStart = next
        lastPlain = null
    }
    while (i < text.length) {
        val c = text[i]
        if (c == '[') {
            val close = text.indexOf("](", i)
            val end = if (close > 0) text.indexOf(')', close + 2) else -1
            if (close > i + 1 && end > close + 2) {
                val target = text.substring(close + 2, end).trim()
                if (target.startsWith("http://") || target.startsWith("https://") || target.startsWith("www.")) {
                    flush(i)
                    withLink(LinkAnnotation.Url(url(target), style)) { append(text.substring(i + 1, close).replace("**", "").replace("`", "")) }
                    skipTo(end + 1)
                    continue
                }
            }
        }
        val boundary = lastPlain == null || lastPlain!!.isWhitespace() || lastPlain == '('
        if (boundary && (text.startsWith("http://", i) || text.startsWith("https://", i) || text.startsWith("www.", i))) {
            val found = BARE.matchAt(text, i)
            if (found != null) {
                var address = found.value
                while (address.isNotEmpty() && address.last() in ".,;:!?)]}") {
                    if (address.last() == ')' && address.count { it == '(' } >= address.count { it == ')' }) break
                    address = address.dropLast(1)
                }
                if (address.length > 6) {
                    flush(i)
                    withLink(LinkAnnotation.Url(url(address), style)) { append(address) }
                    skipTo(i + address.length)
                    continue
                }
            }
        }
        if (c == '*' && text.startsWith("**", i)) {
            val end = text.indexOf("**", i + 2)
            if (end > i + 2) {
                flush(i)
                withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(rich(text.substring(i + 2, end), link)) }
                skipTo(end + 2)
                continue
            }
        }
        // *italic* and _italic_: a single mark, closed on the same line, around a word.
        if ((c == '*' || c == '_') && i + 2 < text.length && !text[i + 1].isWhitespace()) {
            val end = text.indexOf(c, i + 1)
            val wordStart = lastPlain == null || !lastPlain!!.isLetterOrDigit()
            if (end > i + 1 && wordStart && !text[end - 1].isWhitespace() && text.indexOf('\n', i + 1).let { it < 0 || it > end }) {
                flush(i)
                withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(rich(text.substring(i + 1, end), link)) }
                skipTo(end + 1)
                continue
            }
        }
        if (c == '`') {
            val end = text.indexOf('`', i + 1)
            if (end > i + 1) {
                flush(i)
                withStyle(SpanStyle(fontFamily = FontFamily.Monospace, color = Color(0xFFE5C07B), background = Color(0x28FFFFFF))) { append(text.substring(i + 1, end)) }
                skipTo(end + 1)
                continue
            }
        }
        lastPlain = c
        i++
    }
    flush(text.length)
}


// --- Work grouping, as the Claude app shows it ---

private enum class Act { RAN, READ, SEARCHED, EDITED, WROTE, OTHER }

private fun act(name: String) = when (name) {
    "bash" -> Act.RAN
    "read", "list", "web_fetch" -> Act.READ
    "glob", "grep", "web_search" -> Act.SEARCHED
    "edit" -> Act.EDITED
    "write" -> Act.WROTE
    else -> Act.OTHER
}

/** `(added, removed)` from a tool's diff header (`@@ <line> +<added> -<removed>`). */
private fun diffCounts(diff: String): Pair<Int, Int>? {
    if (!diff.startsWith("@@ ")) return null
    val parts = diff.lineSequence().first().removePrefix("@@ ").split(' ')
    val added = parts.getOrNull(1)?.removePrefix("+")?.toIntOrNull() ?: return null
    val removed = parts.getOrNull(2)?.removePrefix("-")?.toIntOrNull() ?: return null
    return added to removed
}

/** The words a file tool says itself before its path ("modifié src/a.rs"): its verb, said once. */
private val SAID_VERBS = listOf("modifié", "créé", "remplacé", "edited", "created", "replaced")

private fun saidVerb(tool: ItemRec): String? {
    if (act(tool.title) != Act.EDITED && act(tool.title) != Act.WROTE) return null
    val text = tool.text.removePrefix(tool.title).trim()
    return SAID_VERBS.firstOrNull { text.startsWith("$it ") }
}

/** What a call worked on: its summary without the tool's name, on one line. */
private fun target(tool: ItemRec): String {
    val said = saidVerb(tool)
    val text = tool.text.removePrefix(tool.title).trim().let { if (said != null) it.removePrefix(said).trim() else it }.removePrefix("$ ").trim()
    val lines = text.lines().filter { it.isNotBlank() }
    return if (lines.size > 1) lines.first().trimEnd() + " …" else lines.firstOrNull().orEmpty()
}

@Composable
private fun verb(tool: ItemRec): String = saidVerb(tool)?.replaceFirstChar { it.uppercase() } ?: when (act(tool.title)) {
    Act.RAN -> stringResource(R.string.tool_ran)
    Act.READ -> stringResource(R.string.tool_read)
    Act.SEARCHED -> stringResource(R.string.tool_searched)
    Act.EDITED -> stringResource(R.string.tool_edited)
    Act.WROTE -> stringResource(R.string.tool_wrote)
    Act.OTHER -> tool.title
}

/** "Lu stats.py" for one call; "2 commandes exécutées, 1 fichier lu" for several. */
@Composable
private fun describeTools(tools: List<ItemRec>): String {
    if (tools.size == 1) {
        val t = target(tools[0])
        val v = verb(tools[0])
        return if (t.isBlank()) v else "$v $t"
    }
    val res = LocalContext.current.resources
    fun part(n: Int, one: Int, many: Int) = if (n == 0) null else res.getString(if (n == 1) one else many, n)
    fun count(a: Act) = tools.count { act(it.title) == a }
    return listOfNotNull(
        part(count(Act.RAN), R.string.tools_ran_one, R.string.tools_ran_many),
        part(count(Act.READ), R.string.tools_read_one, R.string.tools_read_many),
        part(count(Act.SEARCHED), R.string.tools_searched_one, R.string.tools_searched_many),
        part(count(Act.EDITED), R.string.tools_edited_one, R.string.tools_edited_many),
        part(count(Act.WROTE), R.string.tools_wrote_one, R.string.tools_wrote_many),
        part(count(Act.OTHER), R.string.tools_other_one, R.string.tools_other_many),
    ).joinToString(", ").replaceFirstChar { it.uppercase() }
}

/** Three dots that light up in turn, in front of what Po is doing. */
@Composable
private fun WorkingDots(color: Color) {
    val phase by rememberInfiniteTransition(label = "dots").animateFloat(0f, 3f, infiniteRepeatable(tween(1100)), label = "phase")
    Row(horizontalArrangement = Arrangement.spacedBy(3.dp), verticalAlignment = Alignment.CenterVertically) {
        repeat(3) { i ->
            val lit = phase.toInt() == i
            Box(Modifier.size(5.dp).clip(RoundedCornerShape(50)).background(color.copy(alpha = if (lit) 1f else 0.35f)))
        }
    }
}

/** A discreet line of work: its words, a mark of the changes, a chevron; a touch unfolds it. */
@Composable
private fun WorkLine(
    text: String,
    running: Boolean,
    open: Boolean,
    failed: Int = 0,
    counts: Pair<Int, Int>? = null,
    mono: String? = null,
    icon: (@Composable () -> Unit)? = null,
    onClick: () -> Unit,
) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val error = MaterialTheme.colorScheme.error
    // A failed call is common (a grep that finds nothing): counted in red, the line stays calm.
    val color = if (running) PoColors.Amber else muted
    val strong = MaterialTheme.colorScheme.onSurface
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).clickable(onClick = onClick).padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (running) {
            WorkingDots(PoColors.Amber)
            Spacer(Modifier.width(8.dp))
        } else if (icon != null) {
            icon()
            Spacer(Modifier.width(8.dp))
        }
        val failures = if (failed > 0) stringResource(R.string.failures, failed) else null
        Text(
            buildAnnotatedString {
                append(text)
                if (mono != null) {
                    append(" ")
                    withStyle(SpanStyle(fontFamily = FontFamily.Monospace, color = strong)) { append(mono) }
                }
                if (failures != null) withStyle(SpanStyle(color = error)) { append(" · $failures") }
            },
            style = MaterialTheme.typography.bodyMedium,
            color = color,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false),
        )
        counts?.let { (added, removed) ->
            if (added > 0 || removed > 0) {
                Spacer(Modifier.width(8.dp))
                Row(Modifier.clip(RoundedCornerShape(6.dp))) {
                    if (added > 0) Text("+$added", Modifier.background(Color(0x332EA043)).padding(horizontal = 5.dp, vertical = 1.dp), color = Color(0xFF3FB950), style = Mono)
                    if (removed > 0) Text("-$removed", Modifier.background(Color(0x33F85149)).padding(horizontal = 5.dp, vertical = 1.dp), color = Color(0xFFF85149), style = Mono)
                }
            }
        }
        Spacer(Modifier.width(4.dp))
        Icon(PoIcons.Chevron, null, Modifier.size(16.dp).rotate(if (open) 90f else 0f), tint = muted.copy(alpha = 0.8f))
    }
}

/** A plain tap runs [fold]; a long press, a drag or a scroll do not. The touches are watched before the text sees them and never
 *  consumed, so the selection inside (long press) keeps working. */
private fun Modifier.foldOnTap(fold: () -> Unit): Modifier = pointerInput(Unit) {
    awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
        val up = withTimeoutOrNull(viewConfiguration.longPressTimeoutMillis) { waitForUpOrCancellation(PointerEventPass.Initial) }
        if (up != null && (up.position - down.position).getDistance() < viewConfiguration.touchSlop) fold()
    }
}

@Composable
private fun GroupedThinkingRow(group: DisplayItem.GroupedThinking) {
    var open by rememberSaveable(group.id) { mutableStateOf(false) }
    val running = group.items.any { it.streaming }
    val text = group.items.joinToString("\n\n") { it.text.trim() }.trim()
    Column(Modifier.fillMaxWidth().animateContentSize()) {
        WorkLine(
            text = stringResource(if (running) R.string.thinking_in_progress else R.string.thinking_done),
            running = running,
            open = open,
            onClick = { open = !open },
        )
        if (open) {
            // A tap folds it; a long press selects words of it. While the model is still thinking
            // and no text has arrived, a placeholder shows rather than an empty fold.
            Row(Modifier.padding(start = 4.dp, bottom = 6.dp).height(IntrinsicSize.Min).foldOnTap { open = false }) {
                Box(Modifier.width(2.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                Spacer(Modifier.width(10.dp))
                SelectionContainer {
                    Text(
                        if (text.isNotBlank()) text + (if (running) " ▍" else "") else stringResource(R.string.thinking_in_progress),
                        style = MaterialTheme.typography.bodyMedium,
                        fontStyle = FontStyle.Italic,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun GroupedToolsRow(group: DisplayItem.GroupedTools, dynamicAction: String? = null, onTool: (ItemRec) -> Unit) {
    var open by rememberSaveable(group.id) { mutableStateOf(false) }
    val running = group.items.firstOrNull { it.status == ToolStatus.RUNNING }
    val failed = group.items.count { it.status == ToolStatus.ERROR || it.status == ToolStatus.DENIED }
    val counts = group.items.mapNotNull { diffCounts(it.detail) }.fold(0 to 0) { (a, r), (x, y) -> (a + x) to (r + y) }
    val single = group.items.singleOrNull()
    Column(Modifier.fillMaxWidth().animateContentSize()) {
        when {
            // A single call still running: tapping opens its details (the command it is running),
            // not an empty fold. With several, the fold lists them.
            running != null -> WorkLine(
                text = dynamicAction ?: stringResource(R.string.tool_running_one, running.title),
                running = true,
                open = if (single != null) false else open,
                onClick = { if (single != null) onTool(single) else open = !open },
            )
            // One call: its verb, then its target in a monospaced face; a touch opens its details.
            single != null -> WorkLine(
                text = verb(single),
                mono = target(single).ifBlank { null },
                running = false,
                open = false,
                failed = failed,
                counts = counts,
                onClick = { onTool(single) },
            )
            else -> WorkLine(
                text = describeTools(group.items),
                running = false,
                open = open,
                failed = failed,
                counts = counts,
                onClick = { open = !open },
            )
        }
        if (open && single == null) {
            Column(Modifier.padding(start = 12.dp)) {
                group.items.forEach { tool ->
                    WorkLine(
                        text = verb(tool),
                        mono = target(tool).ifBlank { null },
                        running = tool.status == ToolStatus.RUNNING,
                        open = false,
                        failed = if (tool.status == ToolStatus.ERROR || tool.status == ToolStatus.DENIED) 1 else 0,
                        counts = diffCounts(tool.detail),
                        onClick = { onTool(tool) },
                    )
                }
            }
        }
    }
}

/** A call running, in the present: "Exécute cargo test", "Lit src/a.rs". */
@Composable
private fun nowDoing(tool: ItemRec): String {
    val v = when (act(tool.title)) {
        Act.RAN -> stringResource(R.string.now_ran)
        Act.READ -> stringResource(R.string.now_read)
        Act.SEARCHED -> stringResource(R.string.now_searched)
        Act.EDITED -> stringResource(R.string.now_edited)
        Act.WROTE -> stringResource(R.string.now_wrote)
        Act.OTHER -> stringResource(R.string.now_other, tool.title)
    }
    val t = target(tool)
    return if (t.isBlank()) v else "$v $t"
}

/** The end of a text being written, on one line, without its markdown marks. */
private fun tailOf(text: String, room: Int = 90): String {
    val flat = text.split(Regex("\\s+")).map { it.trim('*', '`', '|', '#') }.filter { it.isNotEmpty() && it != "-" }.joinToString(" ")
    return if (flat.length <= room) flat else "…" + flat.takeLast(room)
}

/**
 * The work of a turn at the low level. Live: one line of what the model does now, replaced in
 * place. Answered: nothing, the answer replaced it. A turn cut short: its work folded in one
 * line that a touch opens.
 */
@Composable
private fun LowWorkRow(block: DisplayItem.LowWork, dynamicAction: String?, onTool: (ItemRec) -> Unit) {
    var open by rememberSaveable(block.id) { mutableStateOf(false) }
    if (!block.live && block.answered) return
    val tools = block.items.filter { it.kind == ItemKind.TOOL }
    val res = LocalContext.current.resources
    val count = if (tools.isEmpty()) "" else " · " + res.getString(if (tools.size == 1) R.string.tools_other_one else R.string.tools_other_many, tools.size)
    if (block.live) {
        val last = block.items.last()
        val text = when {
            last.kind == ItemKind.THINKING -> stringResource(R.string.low_thinking) +
                last.text.trim().takeIf { it.isNotBlank() }?.let { " : " + tailOf(it) }.orEmpty()
            last.kind == ItemKind.TOOL && last.status == ToolStatus.RUNNING -> dynamicAction ?: nowDoing(last)
            last.kind == ItemKind.ASSISTANT -> stringResource(R.string.low_writing) +
                last.text.trim().takeIf { it.isNotBlank() }?.let { " : " + tailOf(it) }.orEmpty()
            last.kind == ItemKind.DELEGATION -> stringResource(R.string.low_agent, last.title.substringBefore(" · "))
            else -> stringResource(R.string.low_analysing)
        }
        WorkLine(text = text + count, running = true, open = false, onClick = { last.takeIf { it.kind == ItemKind.TOOL }?.let(onTool) })
        return
    }
    val failed = tools.count { it.status == ToolStatus.ERROR || it.status == ToolStatus.DENIED }
    Column(Modifier.fillMaxWidth().animateContentSize()) {
        WorkLine(
            text = if (tools.isEmpty()) stringResource(R.string.thinking_done) else describeTools(tools),
            running = false,
            open = open,
            failed = failed,
            onClick = { open = !open },
        )
        if (open) {
            Column(Modifier.padding(start = 12.dp)) {
                tools.forEach { tool ->
                    WorkLine(
                        text = verb(tool),
                        mono = target(tool).ifBlank { null },
                        running = false,
                        open = false,
                        failed = if (tool.status == ToolStatus.ERROR || tool.status == ToolStatus.DENIED) 1 else 0,
                        counts = diffCounts(tool.detail),
                        onClick = { onTool(tool) },
                    )
                }
            }
        }
    }
}

@Composable
private fun StatusStrip(status: StatusRec, onHardware: () -> Unit, onContext: () -> Unit) {
    val workspace = status.workspace.trimEnd('/', '\\').substringAfterLast('/').substringAfterLast('\\').takeIf { it.isNotBlank() }
    val branch = status.branch?.takeIf { it.isNotBlank() }
    val hasDiff = status.gitAdded > 0u || status.gitRemoved > 0u

    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
        modifier = Modifier.fillMaxWidth()
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 12.dp, vertical = 3.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp)
        ) {
            if (workspace != null) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(PoIcons.File, null, Modifier.size(12.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                    Spacer(Modifier.width(3.dp))
                    Text(workspace, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }

            if (branch != null) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(PoIcons.Branch, null, Modifier.size(12.dp), tint = MaterialTheme.colorScheme.primary)
                    Spacer(Modifier.width(3.dp))
                    Text(branch, style = MaterialTheme.typography.labelSmall.copy(fontWeight = androidx.compose.ui.text.font.FontWeight.Medium), color = MaterialTheme.colorScheme.primary)
                }
            }

            if (hasDiff) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (status.gitAdded > 0u) {
                        Text("▲${status.gitAdded}", style = MaterialTheme.typography.labelSmall.copy(fontWeight = androidx.compose.ui.text.font.FontWeight.Bold), color = PoColors.Moss)
                        if (status.gitRemoved > 0u) Spacer(Modifier.width(4.dp))
                    }
                    if (status.gitRemoved > 0u) {
                        Text("▼${status.gitRemoved}", style = MaterialTheme.typography.labelSmall.copy(fontWeight = androidx.compose.ui.text.font.FontWeight.Bold), color = PoColors.Red)
                    }
                }
            }

            if (status.contextWindow > 0u) {
                val pct = ((status.contextUsed.toDouble() / status.contextWindow.toDouble()) * 100).toInt()
                val usedK = if (status.contextUsed >= 10_000u) "${status.contextUsed / 1000u}k" else String.format("%.1fk", status.contextUsed.toDouble() / 1000.0)
                val winK = "${status.contextWindow / 1000u}k"
                Text(
                    "$usedK/$winK ($pct%)",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.clickable { onContext() }
                )
            }

            if (status.generationTokS > 0f) {
                Text(
                    "${status.generationTokS.toInt()} tok/s",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )
            }
        }
    }
}


/** The task goals, foldable: a one-line count of what is met, each goal with its mark when open. */
@Composable
private fun GoalsBar(goals: List<GoalRec>) {
    var open by rememberSaveable { mutableStateOf(false) }
    val met = goals.count { it.state == "met" }
    val failed = goals.count { it.state == "failed" }
    val accent = if (met == goals.size) PoColors.Moss else PoColors.Amber
    Surface(
        shape = RoundedCornerShape(10.dp),
        color = accent.copy(alpha = 0.12f),
        border = BorderStroke(1.dp, accent.copy(alpha = 0.35f)),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp).clickable { open = !open }
    ) {
        Column(Modifier.padding(horizontal = 10.dp, vertical = 6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(PoIcons.Goals, null, tint = accent, modifier = Modifier.size(16.dp))
                Spacer(Modifier.width(8.dp))
                Text(
                    stringResource(R.string.goals_summary, met, goals.size) + if (failed > 0) " · " + stringResource(R.string.goals_failed, failed) else "",
                    style = MaterialTheme.typography.bodySmall,
                    fontWeight = FontWeight.SemiBold,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.weight(1f)
                )
                Icon(Icons.Default.KeyboardArrowDown, null, Modifier.size(18.dp).rotate(if (open) 180f else 0f), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (open) {
                Spacer(Modifier.height(4.dp))
                goals.forEach { goal ->
                    val (mark, color) = when (goal.state) {
                        "met" -> "✓" to PoColors.Moss
                        "failed" -> "✗" to MaterialTheme.colorScheme.error
                        else -> "○" to MaterialTheme.colorScheme.onSurfaceVariant
                    }
                    Row(Modifier.padding(vertical = 2.dp)) {
                        Text(mark, color = color, fontWeight = FontWeight.Bold, modifier = Modifier.width(18.dp))
                        Text(goal.text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurface)
                    }
                }
            }
        }
    }
}

@Composable
private fun ContextWarningBanner(state: AppState) {
    val isLoop = state.status?.contextWarning.isNullOrBlank() && !state.status?.loopWarning.isNullOrBlank()
    val warning = state.status?.contextWarning?.takeIf { it.isNotBlank() } ?: state.status?.loopWarning?.takeIf { it.isNotBlank() } ?: return
    var dismissedFor by rememberSaveable(state.status?.model, isLoop) { mutableStateOf<String?>(null) }
    if (dismissedFor == warning) return

    Surface(
        shape = RoundedCornerShape(10.dp),
        color = PoColors.Amber.copy(alpha = 0.14f),
        border = BorderStroke(1.dp, PoColors.Amber.copy(alpha = 0.35f)),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 4.dp)
    ) {
        Row(
            Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Icon(
                PoIcons.Warning,
                null,
                tint = PoColors.Amber,
                modifier = Modifier.size(16.dp)
            )
            Spacer(Modifier.width(8.dp))
            Text(
                warning,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.weight(1f),
                maxLines = 3,
                overflow = TextOverflow.Ellipsis
            )
            Spacer(Modifier.width(6.dp))
            if (!isLoop) {
                OutlinedButton(
                    onClick = {
                        state.send(InputRec.Command("/compact"))
                    },
                    contentPadding = PaddingValues(horizontal = 8.dp, vertical = 2.dp),
                    modifier = Modifier.height(28.dp),
                    shape = RoundedCornerShape(6.dp),
                    border = BorderStroke(1.dp, PoColors.Amber.copy(alpha = 0.6f))
                ) {
                    Text(
                        stringResource(R.string.compact_now),
                        style = MaterialTheme.typography.labelSmall,
                        color = PoColors.Amber,
                        fontWeight = FontWeight.SemiBold
                    )
                }
            }
            Spacer(Modifier.width(4.dp))
            IconButton(
                onClick = { dismissedFor = warning },
                modifier = Modifier.size(24.dp)
            ) {
                Icon(
                    Icons.Default.Close,
                    stringResource(R.string.dismiss_notification),
                    modifier = Modifier.size(14.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant
                )
            }
        }
    }
}
