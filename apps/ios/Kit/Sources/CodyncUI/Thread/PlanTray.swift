import CodyncKit
import SwiftUI

/// The running turn's plan above the composer: folded or unfolded, gone until the next turn once
/// hidden, and a tap on its title opens the turn's steps (Grok Bot's plan tray).
struct PlanTray: View {
    let turn: Int64
    let steps: [PlanItem]
    let openTrace: () -> Void
    @State private var folded = false
    @State private var hiddenTurn: Int64?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var finished: Int { steps.filter { $0.status == "completed" }.count }

    var body: some View {
        if hiddenTurn != turn {
            VStack(alignment: .leading, spacing: 6) {
                header
                if !folded {
                    VStack(alignment: .leading, spacing: 5) {
                        ForEach(Array(steps.enumerated()), id: \.offset) { _, step in
                            row(step)
                        }
                    }
                    .padding(.bottom, 2)
                    .transition(.opacity)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .padding(.horizontal, 16)
            .animation(Motion.reduced(Motion.layout, reduceMotion), value: folded)
            .transition(.opacity.combined(with: .move(edge: .bottom)))
        }
    }

    private var header: some View {
        HStack(spacing: 6) {
            Button(action: openTrace) {
                HStack(spacing: 6) {
                    Image(systemName: "checklist").font(.system(size: 11, weight: .bold))
                    Text(L10n.tr("Plan")).font(.footnote.weight(.semibold)).foregroundStyle(Palette.text)
                    Text("\(finished)/\(steps.count)").font(.footnote.monospacedDigit()).foregroundStyle(Palette.tertiary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Palette.secondary)
            .accessibilityLabel(L10n.tr("Plan, \(finished) of \(steps.count) done. Open the steps."))
            Spacer(minLength: 0)
            Button {
                withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { folded.toggle() }
            } label: {
                Image(systemName: folded ? "chevron.up" : "chevron.down")
                    .font(.system(size: 10, weight: .semibold))
                    .frame(width: 24, height: 24)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Palette.secondary)
            .accessibilityLabel(L10n.tr(folded ? "Unfold the plan" : "Fold the plan"))
            Button { hiddenTurn = turn } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .frame(width: 24, height: 24)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Palette.secondary)
            .accessibilityLabel(L10n.tr("Hide the plan"))
        }
    }

    private func row(_ step: PlanItem) -> some View {
        let done = step.status == "completed"
        return Label {
            Text(step.content)
                .font(.footnote)
                .strikethrough(done)
                .foregroundStyle(done ? Palette.tertiary : Palette.text)
        } icon: {
            Image(systemName: done ? "checkmark.circle.fill" : step.status == "in_progress" ? "circle.dotted" : "circle")
                .font(.system(size: 10))
                .foregroundStyle(done ? Palette.accent : Palette.tertiary)
        }
        .accessibilityElement(children: .combine)
    }
}
