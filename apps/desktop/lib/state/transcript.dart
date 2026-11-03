// The conversation as the app shows it: session history and live events
// reduced into display items. Pure Dart, no widgets, so it's unit-tested.

import 'dart:convert';

import 'package:tern_client/tern_client.dart';

sealed class Item {}

class UserItem extends Item {
  UserItem(this.text);
  final String text;
}

class AssistantItem extends Item {
  AssistantItem(this.text, {this.streaming = false});
  String text;
  bool streaming;
}

enum StepStatus { running, ok, error }

class ToolStep {
  ToolStep({required this.id, required this.name, required this.summary});
  final String id;
  final String name;
  String summary;
  StepStatus status = StepStatus.running;
  String output = '';
  DateTime? started;
  DateTime? finished;
}

/// Consecutive tool calls, shown as one card.
class WorkItem extends Item {
  WorkItem({this.runId});

  /// The run these calls belong to, for the run inspector.
  String? runId;
  final steps = <ToolStep>[];
  DateTime? started;
  DateTime? finished;

  bool get running => steps.any((s) => s.status == StepStatus.running);
  Duration? get elapsed => started == null || finished == null
      ? null
      : finished!.difference(started!);
}

enum PillKind { routine, connector, memory, compacted, retrying }

/// A quiet one-line note: something the agent made, or the engine did.
class PillItem extends Item {
  PillItem(this.kind, this.text);
  final PillKind kind;
  final String text;
}

class ApprovalItem extends Item {
  ApprovalItem(this.approval);
  final Approval approval;

  /// Null while waiting. `approved`, `denied`, `timeout`, or the decision
  /// this app sent.
  String? outcome;
}

/// A run that ended other than by completing.
class RunEndItem extends Item {
  RunEndItem({required this.runId, required this.status, this.error});
  final String runId;
  final String status;
  final String? error;
}

class Transcript {
  final items = <Item>[];

  /// The run in progress here, if any.
  String? activeRun;

  /// Rebuild from stored history (the daemon's view, after compaction
  /// summaries and tool results).
  static Transcript fromSession(SessionDetail s) {
    final t = Transcript();
    final steps = <String, ToolStep>{};
    for (final m in s.messages) {
      if (m.summary) {
        t.items.add(
          PillItem(PillKind.compacted, 'Earlier messages summarized'),
        );
        continue;
      }
      for (final b in m.content) {
        switch (b) {
          case TextBlock(:final text) when text.trim().isNotEmpty:
            t.items.add(
              m.role == 'user' ? UserItem(text) : AssistantItem(text),
            );
          case TextBlock():
            break;
          case ToolUseBlock(:final id, :final name, :final input):
            final step = ToolStep(
              id: id,
              name: name,
              summary: summarize(name, input),
            );
            steps[id] = step;
            t._work(m.runId).steps.add(step);
          case ToolResultBlock(
            :final toolUseId,
            :final content,
            :final isError,
          ):
            final step = steps[toolUseId];
            if (step == null) break;
            step
              ..status = isError ? StepStatus.error : StepStatus.ok
              ..output = content;
            if (!isError) t._pillFor(step.name, step.summary);
        }
      }
    }
    return t;
  }

  /// Apply one live event. Returns false if it changed nothing.
  bool apply(TernEvent e, {DateTime? now}) {
    now ??= DateTime.now();
    switch (e) {
      case RunStarted(:final runId):
        activeRun = runId;
      case TextDelta(:final text):
        final last = items.lastOrNull;
        if (last is AssistantItem && last.streaming) {
          last.text += text;
        } else {
          items.add(AssistantItem(text, streaming: true));
        }
      case AssistantMessage(:final text, :final replayed):
        final last = items.lastOrNull;
        if (last is AssistantItem && last.streaming) {
          last
            ..text = text
            ..streaming = false;
        } else if (text.trim().isNotEmpty &&
            !(replayed && _hasAssistant(text))) {
          items.add(AssistantItem(text));
        }
        // A turn with only tool calls streams nothing worth keeping.
        if (items.lastOrNull case AssistantItem(:final text)
            when text.trim().isEmpty) {
          items.removeLast();
        }
      case ToolStarted(:final id, :final name, :final summary):
        if (_step(id) != null) return false;
        final work = _work(e.runId);
        work.started ??= now;
        work.finished = null;
        work.steps.add(
          ToolStep(id: id, name: name, summary: summary)..started = now,
        );
      case ToolFinished(:final id, :final name, :final isError, :final output):
        var step = _step(id);
        if (step == null) {
          // Started before we were watching (or replayed after a crash).
          step = ToolStep(id: id, name: name, summary: name)..started = now;
          _work(e.runId).steps.add(step);
        }
        step
          ..status = isError ? StepStatus.error : StepStatus.ok
          ..output = output
          ..finished = now;
        final work = items.whereType<WorkItem>().lastWhere(
          (w) => w.steps.contains(step),
        );
        if (!work.running) work.finished = now;
        if (!isError) _pillFor(name, step.summary);
      case ApprovalRequested(:final approval):
        if (items.any(
          (i) => i is ApprovalItem && i.approval.id == approval.id,
        )) {
          return false;
        }
        items.add(ApprovalItem(approval));
      case ApprovalAnswered(:final outcome, :final runId):
        final pending = items.whereType<ApprovalItem>().where(
          (a) => a.approval.runId == runId && a.outcome == null,
        );
        if (pending.isEmpty) return false;
        pending.first.outcome = outcome;
      case Retrying(:final attempt, :final delayMs):
        items.add(
          PillItem(
            PillKind.retrying,
            'Retrying (attempt $attempt) in ${(delayMs / 1000).toStringAsFixed(1)}s',
          ),
        );
      case Compacted():
        items.add(PillItem(PillKind.compacted, 'Earlier messages summarized'));
      case RunFinished(:final runId, :final status, :final error):
        if (activeRun == runId) activeRun = null;
        for (final i in items) {
          if (i is AssistantItem) i.streaming = false;
          if (i is ApprovalItem &&
              i.outcome == null &&
              i.approval.runId == runId) {
            i.outcome = 'expired';
          }
          if (i is WorkItem) {
            for (final s in i.steps) {
              if (s.status == StepStatus.running) s.status = StepStatus.error;
            }
            i.finished ??= now;
          }
        }
        if (status != 'completed') {
          items.add(RunEndItem(runId: runId, status: status, error: error));
        }
      case UsageEvent() || UnknownEvent():
        return false;
    }
    return true;
  }

  /// The work card to add steps to: the last one, if only notes follow it
  /// (so a "Saved to memory" note doesn't split a run's tools in two).
  WorkItem _work([String? runId]) {
    for (final i in items.reversed) {
      if (i is WorkItem) return i..runId ??= runId;
      if (i is! PillItem) break;
    }
    final w = WorkItem(runId: runId);
    items.add(w);
    return w;
  }

  ToolStep? _step(String id) {
    for (final w in items.whereType<WorkItem>()) {
      for (final s in w.steps) {
        if (s.id == id) return s;
      }
    }
    return null;
  }

  bool _hasAssistant(String text) =>
      items.any((i) => i is AssistantItem && i.text == text);

  void _pillFor(String tool, String summary) {
    final pill = switch (tool) {
      'schedule_create' => PillItem(PillKind.routine, 'Created a routine'),
      'connector_activate' => PillItem(
        PillKind.connector,
        'Activated a connector',
      ),
      'memory_save' => PillItem(PillKind.memory, 'Saved to memory'),
      _ => null,
    };
    if (pill != null) items.add(pill);
  }
}

/// A one-line description of a tool call from its input, for history
/// (live events carry the tool's own summary).
String summarize(String name, Object? input) {
  if (input is Map) {
    for (final key in const ['command', 'path', 'url', 'query', 'name']) {
      if (input[key] case final String v) return '$name ${_clip(v)}';
    }
    for (final v in input.values) {
      if (v is String) return '$name ${_clip(v)}';
    }
    if (input.isNotEmpty) return '$name ${_clip(jsonEncode(input))}';
  }
  return name;
}

String _clip(String s) {
  final line = s.split('\n').first;
  return line.length > 80 ? '${line.substring(0, 79)}…' : line;
}
