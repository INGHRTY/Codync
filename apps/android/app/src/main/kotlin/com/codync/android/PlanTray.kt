package com.codync.android

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import com.kyant.shapes.RoundedRectangle

/**
 * The running turn's plan above the composer: folded or unfolded, gone until the next turn once hidden,
 * and a tap on its title opens the turn's steps (iOS `PlanTray`, the desktop's `PlanTray.tsx`).
 */
@Composable internal fun PlanTray(turn: Long, steps: List<PlanStep>, openTrace: () -> Unit) {
    var folded by rememberSaveable { mutableStateOf(false) }
    var hiddenTurn by rememberSaveable { mutableStateOf<Long?>(null) }
    if (hiddenTurn == turn) return
    val done = steps.count { it.status == "completed" }
    val openLabel = tr("Plan, $done of ${steps.size} done. Open the steps.")
    Column(Modifier.padding(horizontal = 16.dp, vertical = 6.dp).fillMaxWidth()
        .clip(RoundedRectangle(16.dp)).background(Palette.surface).padding(horizontal = 12.dp, vertical = 9.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(Modifier.weight(1f).padding(vertical = 3.dp).plainButton { openTrace() }
                .semantics { contentDescription = openLabel }, verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Symbol("checklist", size = 13.dp, tint = Palette.secondary)
                Text(tr("Plan"), style = Type.footnote.semibold, color = Palette.text)
                Text("$done/${steps.size}", style = Type.footnote, color = Palette.tertiary)
            }
            IconButton(if (folded) "Unfold the plan" else "Fold the plan", if (folded) "chevron.up" else "chevron.down",
                Modifier.width(28.dp), tint = Palette.secondary) { folded = !folded }
            IconButton("Hide the plan", "xmark", Modifier.width(28.dp), tint = Palette.secondary) { hiddenTurn = turn }
        }
        AnimatedVisibility(!folded, enter = fadeIn(Motion.fade()) + expandVertically(Motion.layoutSize),
            exit = fadeOut(Motion.fade()) + shrinkVertically(Motion.layoutSize)) {
            Column(Modifier.padding(top = 4.dp, bottom = 2.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                for (step in steps) PlanStepRow(step)
            }
        }
    }
}

@Composable private fun PlanStepRow(step: PlanStep) {
    val done = step.status == "completed"
    SelectionContainer {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Box(Modifier.width(12.dp)) {
                Symbol(when (step.status) {
                    "completed" -> "checkmark.circle.fill"
                    "in_progress" -> "circle.dotted"
                    else -> "circle"
                }, size = 11.dp, tint = if (done) Palette.accent else Palette.tertiary)
            }
            Text(step.content, style = Type.footnote.copy(textDecoration = if (done) TextDecoration.LineThrough else null),
                color = if (done) Palette.tertiary else Palette.text)
        }
    }
}
