package dev.po.remote.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.po.remote.AppState
import dev.po.remote.BuildConfig
import dev.po.remote.R
import dev.po.remote.Screen
import uniffi.po_mobile.LinkState
import uniffi.po_mobile.PcInfo
import uniffi.po_mobile.SessionRec

@Composable
fun App(state: AppState) {
    // Nothing pops up over the screen: what the app has to say passes in a thin line at the top,
    // and goes by itself.
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.error) {
        if (state.error != null) {
            kotlinx.coroutines.delay(6000)
            state.error = null
        }
    }
    LaunchedEffect(state.toast) {
        if (state.toast != null) {
            kotlinx.coroutines.delay(3500)
            state.toast = null
        }
    }
    Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
        when (val screen = state.screen) {
            Screen.SignIn -> SignInScreen(state, snackbar)
            Screen.Home -> HomeScreen(state, snackbar)
            is Screen.Project -> ProjectScreen(state, screen, snackbar)
            is Screen.Conversation -> ConversationScreen(state, screen, snackbar)
            Screen.Settings -> SettingsScreen(state, snackbar)
        }
        (state.error ?: state.toast)?.let { text ->
            val warn = state.error != null
            Surface(
                Modifier.align(Alignment.TopCenter).statusBarsPadding().padding(top = 60.dp, start = 24.dp, end = 24.dp).clickable { state.error = null; state.toast = null },
                shape = CircleShape,
                color = if (warn) MaterialTheme.colorScheme.errorContainer else MaterialTheme.colorScheme.secondaryContainer,
                shadowElevation = 2.dp,
            ) {
                Text(text, Modifier.padding(horizontal = 14.dp, vertical = 6.dp), style = MaterialTheme.typography.labelLarge, maxLines = 2, overflow = TextOverflow.Ellipsis, color = if (warn) MaterialTheme.colorScheme.onErrorContainer else MaterialTheme.colorScheme.onSecondaryContainer)
            }
        }
        state.busyText?.let { text ->
            Surface(Modifier.align(Alignment.BottomCenter).padding(bottom = 96.dp), shape = MaterialTheme.shapes.large, tonalElevation = 6.dp, shadowElevation = 6.dp) {
                Row(Modifier.padding(horizontal = 18.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(12.dp))
                    Text(text)
                }
            }
        }
    }
}

@Composable
private fun SignInScreen(state: AppState, snackbar: SnackbarHostState) {
    val context = LocalContext.current
    Scaffold(snackbarHost = { SnackbarHost(snackbar) }) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(28.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Mascot(120.dp)
            Spacer(Modifier.height(20.dp))
            Text(stringResource(R.string.welcome_title), style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Bold)
            Spacer(Modifier.height(10.dp))
            Text(stringResource(R.string.welcome_text), textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.height(28.dp))
            val code = state.signIn
            if (code == null) {
                Button(onClick = { state.beginSignIn() }, enabled = state.busyText == null) { Text(stringResource(R.string.sign_in)) }
            } else {
                Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)) {
                    Column(Modifier.padding(20.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(stringResource(R.string.sign_in_code))
                        Spacer(Modifier.height(8.dp))
                        Text(code.code, fontSize = 30.sp, fontWeight = FontWeight.Bold, letterSpacing = 3.sp)
                        Spacer(Modifier.height(12.dp))
                        Button(onClick = { copyAndOpen(context, code.code, code.url) }) { Text(stringResource(R.string.sign_in_open)) }
                        Spacer(Modifier.height(8.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(8.dp))
                            Text(stringResource(R.string.sign_in_waiting), style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
            }
            Spacer(Modifier.height(20.dp))
            Text(stringResource(R.string.sign_in_note), style = MaterialTheme.typography.bodySmall, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(onClick = { state.go(Screen.Settings) }) { Text(stringResource(R.string.settings)) }
        }
    }
}

private fun copyAndOpen(context: Context, code: String, url: String) {
    val clipboard = context.getSystemService(android.content.ClipboardManager::class.java)
    clipboard?.setPrimaryClip(android.content.ClipData.newPlainText("code", code))
    runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HomeScreen(state: AppState, snackbar: SnackbarHostState) {
    // Read again every few seconds until a PC can be reached: the PC accepts a new phone on
    // its own, within its next look at the directory.
    LaunchedEffect(Unit) {
        state.refresh()
        while (true) {
            kotlinx.coroutines.delay(5000)
            if (!state.ready || state.pcs.none { it.reachable }) state.refresh()
        }
    }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Mascot(30.dp)
                        Spacer(Modifier.width(10.dp))
                        Column {
                            Text("Po", fontWeight = FontWeight.Bold)
                            Text(state.account ?: "", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                },
                actions = {
                    IconButton(onClick = { state.refresh() }) { Icon(Icons.Default.Refresh, stringResource(R.string.refresh)) }
                    IconButton(onClick = { state.go(Screen.Settings) }) { Icon(Icons.Default.Settings, stringResource(R.string.settings)) }
                },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            if (state.pcs.isEmpty()) {
                item {
                    if (state.refreshing) Waiting(stringResource(R.string.loading)) else Empty(stringResource(R.string.no_pc))
                }
            }
            if (!state.ready && state.pcs.isNotEmpty()) {
                item { Hint(stringResource(R.string.waiting_for_pc)) }
            }
            items(state.pcs, key = { it.id }) { pc -> PcCard(state, pc) }
        }
    }
}

@Composable
private fun PcCard(state: AppState, pc: PcInfo) {
    val link = state.links[pc.id]
    Card(Modifier.fillMaxWidth(), shape = RoundedCornerShape(18.dp)) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                val dot = when (link) {
                    is LinkState.Up -> PoColors.Moss
                    is LinkState.Connecting -> PoColors.Amber
                    else -> if (pc.reachable) Color.Gray else PoColors.Red
                }
                Box(Modifier.size(10.dp).clip(CircleShape).background(dot))
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text(pc.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    // Simple words: connected or not; how, the user has no need to know.
                    Text(
                        when (link) {
                            is LinkState.Up -> stringResource(R.string.link_up)
                            is LinkState.Connecting -> stringResource(R.string.link_connecting)
                            is LinkState.Down -> stringResource(R.string.link_down)
                            null -> if (pc.reachable) stringResource(R.string.seen, ago(pc.seen)) else stringResource(R.string.pc_not_ready)
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 2,
                    )
                }
                if (link is LinkState.Connecting || state.projectsLoading[pc.id] == true) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
            }
            val projects = state.projects[pc.id]
            if (projects != null) {
                Spacer(Modifier.height(10.dp))
                HorizontalDivider()
                if (projects.isEmpty()) Text(stringResource(R.string.no_project), Modifier.padding(top = 10.dp), style = MaterialTheme.typography.bodySmall)
                projects.forEach { p ->
                    Row(
                        Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).clickable { state.openProject(pc.id, p) }.padding(vertical = 10.dp, horizontal = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(p.name, fontWeight = FontWeight.Medium)
                            Text(p.path, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        }
                        if (p.live > 0u) {
                            Badge(stringResource(R.string.live_count, p.live.toInt()), MaterialTheme.colorScheme.tertiary)
                            if (p.saved > 0u) Spacer(Modifier.width(6.dp))
                        }
                        if (p.saved > 0u) {
                            Badge(p.saved.toString(), MaterialTheme.colorScheme.secondary)
                        }
                    }
                }
            } else if (state.projectsLoading[pc.id] == true) {
                Text(stringResource(R.string.loading_projects), Modifier.padding(top = 10.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else if (pc.reachable) {
                TextButton(onClick = { state.loadProjects(pc.id) }) { Text(stringResource(R.string.connect)) }
            }
        }
    }
}

@Composable
fun Badge(text: String, color: Color) {
    Surface(shape = CircleShape, color = color.copy(alpha = 0.16f)) {
        Text(text, Modifier.padding(horizontal = 8.dp, vertical = 2.dp), color = color, style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
fun Empty(text: String) {
    Column(Modifier.fillMaxWidth().padding(top = 60.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Mascot(80.dp)
        Spacer(Modifier.height(14.dp))
        Text(text, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** Something is on its way: a spinner, and what it is. */
@Composable
fun Waiting(text: String) {
    Column(Modifier.fillMaxWidth().padding(top = 80.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        CircularProgressIndicator(Modifier.size(38.dp), strokeWidth = 3.dp)
        Spacer(Modifier.height(14.dp))
        Text(text, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
fun Hint(text: String) {
    Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)) {
        Text(text, Modifier.padding(14.dp), style = MaterialTheme.typography.bodyMedium)
    }
}

fun ago(seconds: ULong): String {
    val delta = (System.currentTimeMillis() / 1000 - seconds.toLong()).coerceAtLeast(0)
    return when {
        delta < 60 -> "< 1 min"
        delta < 3600 -> "${delta / 60} min"
        delta < 86400 -> "${delta / 3600} h"
        else -> "${delta / 86400} j"
    }
}

/** Sessions of the last days first; the older ones folded, at hand. */
private const val RECENT = 8

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
private fun ProjectScreen(state: AppState, screen: Screen.Project, snackbar: SnackbarHostState) {
    var older by remember { mutableStateOf(false) }
    var forgetting by remember { mutableStateOf<SessionRec?>(null) }
    LaunchedEffect(screen.pc, screen.path) {
        state.loadSessions(screen.pc, screen.path)
    }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = { Column { Text(screen.name); Text(screen.path, style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis) } },
                navigationIcon = { IconButton(onClick = { state.back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
                actions = {
                    IconButton(onClick = { state.loadSessions(screen.pc, screen.path) }) { Icon(Icons.Default.Refresh, stringResource(R.string.refresh)) }
                    IconButton(onClick = { state.go(Screen.Settings) }) { Icon(Icons.Default.Settings, stringResource(R.string.settings)) }
                },
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(onClick = { state.newSession(screen.pc, screen.path) }, icon = { Icon(Icons.Default.Add, null) }, text = { Text(stringResource(R.string.new_session)) })
        },
    ) { padding ->
        val all = state.sessions.filter { it.project.replace('\\', '/').equals(screen.path.replace('\\', '/'), ignoreCase = true) }
        val live = all.filter { it.live }
        val saved = all.filter { !it.live }
        val recent = saved.take(RECENT)
        val rest = saved.drop(RECENT)
        LazyColumn(Modifier.fillMaxSize().padding(padding), contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 96.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (state.sessionsLoading && all.isEmpty()) {
                item { Waiting(stringResource(R.string.loading_sessions)) }
            } else if (all.isEmpty()) {
                item { Empty(stringResource(R.string.no_session)) }
            }
            if (live.isNotEmpty()) {
                item { Section(stringResource(R.string.sessions_open)) }
                items(live, key = { it.key }) { s -> SessionRow(s, onLong = null) { state.openSaved(screen.pc, s) } }
            }
            if (recent.isNotEmpty()) {
                item { Section(stringResource(R.string.sessions_recent)) }
                items(recent, key = { it.key }) { s -> SessionRow(s, onLong = { forgetting = s }) { state.openSaved(screen.pc, s) } }
            }
            if (rest.isNotEmpty()) {
                item {
                    Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).clickable { older = !older }.padding(vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(stringResource(R.string.sessions_older, rest.size), Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        Icon(Icons.Default.KeyboardArrowDown, null, Modifier.rotate(if (older) 180f else 0f))
                    }
                }
                if (older) items(rest, key = { it.key }) { s -> SessionRow(s, onLong = { forgetting = s }) { state.openSaved(screen.pc, s) } }
            }
            if (saved.isNotEmpty()) item { Text(stringResource(R.string.sessions_hint), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 6.dp)) }
        }
    }
    forgetting?.let { s ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text(stringResource(R.string.forget_title)) },
            text = { Text(stringResource(R.string.forget_text, s.title.ifBlank { stringResource(R.string.untitled) })) },
            confirmButton = { Button(onClick = { forgetting = null; state.forget(screen.pc, s) }) { Text(stringResource(R.string.forget)) } },
            dismissButton = { TextButton(onClick = { forgetting = null }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

@Composable
private fun Section(text: String) {
    Text(text, Modifier.padding(top = 8.dp, bottom = 2.dp), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SessionRow(s: SessionRec, onLong: (() -> Unit)?, onClick: () -> Unit) {
    Card(Modifier.fillMaxWidth().clip(CardDefaults.shape).combinedClickable(onClick = onClick, onLongClick = onLong)) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(s.title.ifBlank { stringResource(R.string.untitled) }, fontWeight = FontWeight.Medium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                val detail = listOfNotNull(
                    s.model.takeIf { it.isNotBlank() },
                    pluralStringResource(R.plurals.exchanges, s.exchanges.toInt(), s.exchanges.toInt()),
                    s.updated.takeIf { it.isNotBlank() }?.take(16)?.replace('T', ' '),
                ).joinToString(" · ")
                Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            when {
                s.loading -> Badge(stringResource(R.string.loading_model), PoColors.Amber)
                s.busy -> Badge(stringResource(R.string.working), PoColors.Amber)
                s.live && s.headless -> Badge(stringResource(R.string.from_phone), MaterialTheme.colorScheme.primary)
                s.live -> Badge(stringResource(R.string.in_terminal), MaterialTheme.colorScheme.tertiary)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsScreen(state: AppState, snackbar: SnackbarHostState) {
    val context = LocalContext.current
    val prefs = remember { context.getSharedPreferences("po", Context.MODE_PRIVATE) }
    var api by remember { mutableStateOf(prefs.getString("github_api", "") ?: "") }
    var web by remember { mutableStateOf(prefs.getString("github_web", "") ?: "") }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.settings)) },
                navigationIcon = { IconButton(onClick = { state.back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back)) } },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(PoIcons.Palette, null, tint = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.width(8.dp))
                Text(stringResource(R.string.theme), style = MaterialTheme.typography.titleMedium)
            }
            AppTheme.entries.forEach { theme ->
                ThemeRow(theme, selected = state.theme == theme) { state.chooseTheme(theme) }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Appearance.entries.forEach { a ->
                    val on = state.appearance == a
                    Surface(
                        Modifier.clip(CircleShape).clickable { state.chooseAppearance(a) },
                        shape = CircleShape,
                        color = if (on) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceVariant,
                    ) {
                        Text(stringResource(a.label), Modifier.padding(horizontal = 14.dp, vertical = 8.dp), color = if (on) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface)
                    }
                }
            }
            Text(stringResource(R.string.theme_note), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)

            HorizontalDivider()
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Default.Settings, null, tint = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.width(8.dp))
                Text(stringResource(R.string.verbosity), style = MaterialTheme.typography.titleMedium)
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Verbosity.entries.forEach { v ->
                    val on = state.verbosity == v
                    Surface(
                        Modifier.clip(CircleShape).clickable { state.chooseVerbosity(v) },
                        shape = CircleShape,
                        color = if (on) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceVariant,
                    ) {
                        Text(stringResource(v.label), Modifier.padding(horizontal = 14.dp, vertical = 8.dp), color = if (on) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface)
                    }
                }
            }
            Text(stringResource(state.verbosity.hint), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            HorizontalDivider()
            Text(stringResource(R.string.account), style = MaterialTheme.typography.titleMedium)
            Text(state.account ?: stringResource(R.string.not_signed_in))
            if (state.account != null) {
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    OutlinedButton(onClick = { state.signOut() }) { Text(stringResource(R.string.sign_out)) }
                    OutlinedButton(onClick = { state.signOut(); state.beginSignIn() }) { Text(stringResource(R.string.switch_account)) }
                }
                Text(stringResource(R.string.sign_out_note), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (BuildConfig.DEBUG) {
                HorizontalDivider()
                Text(stringResource(R.string.this_phone), style = MaterialTheme.typography.titleMedium)
                Text(dev.po.remote.PoApp.instance.core.deviceId(), style = Mono)
                Text(stringResource(R.string.debug_github), style = MaterialTheme.typography.titleMedium)
                OutlinedTextField(api, { api = it }, Modifier.fillMaxWidth(), label = { Text("API") }, placeholder = { Text("http://10.0.2.2:18998") })
                OutlinedTextField(web, { web = it }, Modifier.fillMaxWidth(), label = { Text("Web") }, placeholder = { Text("http://10.0.2.2:18998") })
                Button(onClick = {
                    prefs.edit().putString("github_api", api.trim()).putString("github_web", web.trim()).apply()
                    state.toast = context.getString(R.string.restart_to_apply)
                }) { Text(stringResource(R.string.save)) }
            }
            Spacer(Modifier.height(8.dp))
            Text("Po ${BuildConfig.VERSION_NAME}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/** A theme to choose, shown in its own colors. */
@Composable
private fun ThemeRow(theme: AppTheme, selected: Boolean, onClick: () -> Unit) {
    PoTheme(theme, dev.po.remote.PoApp.instance.state.appearance) {
        val shape = RoundedCornerShape(14.dp)
        Surface(
            Modifier.fillMaxWidth().clip(shape).border(if (selected) 2.dp else 1.dp, if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outline, shape).clickable(onClick = onClick),
            shape = shape,
            color = MaterialTheme.colorScheme.background,
        ) {
            Row(Modifier.padding(horizontal = 14.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                listOf(MaterialTheme.colorScheme.primary, LocalPo.current.work, MaterialTheme.colorScheme.tertiary, LocalPo.current.link).forEach { c ->
                    Box(Modifier.size(18.dp).clip(CircleShape).background(c).border(1.dp, MaterialTheme.colorScheme.outline, CircleShape))
                    Spacer(Modifier.width(4.dp))
                }
                Spacer(Modifier.width(8.dp))
                Text(stringResource(theme.label), Modifier.weight(1f), color = MaterialTheme.colorScheme.onBackground, fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal)
                if (selected) Icon(Icons.Default.Check, null, tint = MaterialTheme.colorScheme.primary)
            }
        }
    }
}
