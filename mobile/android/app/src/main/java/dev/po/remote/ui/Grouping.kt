package dev.po.remote.ui

import uniffi.po_mobile.ItemKind
import uniffi.po_mobile.ItemRec

// How the conversation's rows group into what is shown, by verbosity. Kept apart from the
// composables: plain Kotlin, tested on the JVM (GroupingTest).

internal sealed interface DisplayItem {
    val id: String
    /** `last`: an answer that ends its turn (only then copy and read-aloud are offered: not under each progress update). */
    data class Single(val item: ItemRec, override val id: String, val last: Boolean = true) : DisplayItem
    data class GroupedThinking(val items: List<ItemRec>, override val id: String) : DisplayItem
    data class GroupedTools(val items: List<ItemRec>, override val id: String) : DisplayItem
    /**
     * Low level: everything a turn does before its final answer - reasoning, tools, subagents,
     * images, messages in between. `live`: the turn runs, one line says what happens now.
     * `answered`: the final answer is there, the work takes no room.
     */
    data class LowWork(val items: List<ItemRec>, override val id: String, val live: Boolean, val answered: Boolean) : DisplayItem
}

/**
 * Between two messages, the reasoning folds into one line and the tool calls into another, said
 * in words ("2 commandes exécutées, 1 fichier lu"). Only the verbose level shows every step.
 */
internal fun groupRows(rows: List<ItemRec>, verbosity: Verbosity, running: Boolean = false): List<DisplayItem> {
    if (verbosity == Verbosity.Low) return lowRows(rows, running)
    // Medium: one line for the reasoning, one for the tools of a step. High: every step its own
    // card, open (WorkBlock's initialOpen).
    val grouped = verbosity == Verbosity.Medium
    val result = mutableListOf<DisplayItem>()
    val thinking = mutableListOf<ItemRec>()
    val tools = mutableListOf<ItemRec>()
    var thinkingStart = -1
    var toolsStart = -1

    fun flush() {
        if (thinking.isNotEmpty()) result.add(DisplayItem.GroupedThinking(thinking.toList(), "thinking-$thinkingStart"))
        if (tools.isNotEmpty()) result.add(DisplayItem.GroupedTools(tools.toList(), "tools-$toolsStart"))
        thinking.clear()
        tools.clear()
        thinkingStart = -1
        toolsStart = -1
    }

    for ((index, row) in rows.withIndex()) {
        if (!grouped && (row.kind == ItemKind.THINKING || row.kind == ItemKind.TOOL)) {
            flush()
            result.add(DisplayItem.Single(row, "${row.kind.name.lowercase()}-$index"))
            continue
        }
        when (row.kind) {
            ItemKind.THINKING -> {
                if (thinkingStart < 0) thinkingStart = index
                thinking.add(row)
            }
            ItemKind.TOOL -> {
                if (toolsStart < 0) toolsStart = index
                tools.add(row)
            }
            else -> {
                flush()
                result.add(DisplayItem.Single(row, "${row.kind.name.lowercase()}-$index"))
            }
        }
    }
    flush()
    // An answer followed, before the next message of the user, by another answer is an update.
    var answerSeen = false
    for (i in result.indices.reversed()) {
        val block = result[i] as? DisplayItem.Single ?: continue
        when (block.item.kind) {
            ItemKind.USER -> answerSeen = false
            ItemKind.ASSISTANT -> {
                if (answerSeen) result[i] = block.copy(last = false)
                answerSeen = true
            }
            else -> {}
        }
    }
    return result
}

private fun isWork(kind: ItemKind) = kind == ItemKind.THINKING || kind == ItemKind.TOOL || kind == ItemKind.DELEGATION

/** The low level, turn by turn (a turn starts at a message of the user), as in the terminal. */
internal fun lowRows(rows: List<ItemRec>, running: Boolean): List<DisplayItem> {
    val result = mutableListOf<DisplayItem>()
    var start = 0
    while (start < rows.size) {
        var end = start + 1
        while (end < rows.size && rows[end].kind != ItemKind.USER) end++
        val lastTurn = end == rows.size
        // The final answer: the last message with no work after it. While the turn runs, a
        // message still being written at the very end may yet be followed by a tool.
        val lastWork = (start until end).lastOrNull { isWork(rows[it].kind) } ?: -1
        val answer = (start until end).lastOrNull { it > lastWork && rows[it].kind == ItemKind.ASSISTANT }
            ?.takeIf { !(running && lastTurn && it == rows.size - 1) }
        val work = mutableListOf<ItemRec>()
        var workStart = -1
        fun flush(live: Boolean) {
            if (work.isNotEmpty()) result.add(DisplayItem.LowWork(work.toList(), "low-$workStart", live, answer != null))
            work.clear()
            workStart = -1
        }
        for (i in start until end) {
            val row = rows[i]
            val merged = i != answer && (isWork(row.kind) || row.kind == ItemKind.ASSISTANT || row.kind == ItemKind.IMAGE)
            if (merged) {
                if (workStart < 0) workStart = i
                work.add(row)
            } else {
                flush(false)
                result.add(DisplayItem.Single(row, "${row.kind.name.lowercase()}-$i"))
            }
        }
        flush(running && lastTurn)
        start = end
    }
    return result
}
