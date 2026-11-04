// The conversation as the app shows it: session history and live events
// reduced into display items. Pure Dart, no widgets, so it's unit-tested.

import 'dart:convert';

import 'package:tern_client/tern_client.dart';

sealed class Item {}

class UserItem extends Item {
  UserItem(this.text, {this.messageId, this.queued = false});
  final String text;

  /// The stored message, when it came from history (side chats branch
  /// from it).
  final int? messageId;

  /// Sent while a run was working: it reads it at its next step.
  bool queued;
}

class AssistantItem extends Item {
  AssistantItem(this.text, {this.streaming = false, this.messageId});
  String text;
  bool streaming;
  final int? messageId;
}

/// Where Home's stream was cut into a new session (docs/channels-design.md
/// §2.6), with what was carried over.
class CutItem extends Item {
  CutItem(this.cut);
  final StreamCut cut;

  /// Showing the carried-over summary.
  bool expanded = false;
}

/// Something posted from elsewhere: a routine's result, a card.
class PostItem extends Item {
  PostItem(this.post);
  Post post;
}

/// A side chat branched off the stream; it opens beside it.
class SideChatItem extends Item {
  SideChatItem(this.chat);
  final StreamSideChat chat;
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

/// One sub-agent, as its parent's agents card shows it.
class AgentChild {
  AgentChild({required this.callId, required this.label, this.task = ''});

  /// The `agent_spawn` call that started it.
  final String callId;
  String label;
  String task;
  String? runId;
  String? sessionId;
  String? model;

  /// The agent profile it runs as (an id, or the name the parent gave
  /// `agent_spawn`), when its parent delegated to one.
  String? agent;

  /// The daemon's word: `starting` (spawn in flight), `queued`, `running`,
  /// `question`, `completed`, `failed`, `cancelled`, `stopped`,
  /// `interrupted`.
  String status = 'starting';
  String? result;
  String? error;
  String? question;
  DateTime? started;
  DateTime? finished;

  bool get active =>
      const {'starting', 'queued', 'running', 'question'}.contains(status);
}

/// The sub-agents a run started in one go, shown as one card.
class AgentsItem extends Item {
  AgentsItem({this.runId});
  String? runId;
  final children = <AgentChild>[];
}

enum PillKind { routine, connector, memory, compacted, retrying, posts }

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

/// The agent asking for secrets. The values never come here: the card
/// holds them in its text fields only until they are sent.
class SecretRequestItem extends Item {
  SecretRequestItem(this.request);
  final SecretRequest request;

  /// Null while waiting. `saved`, `skipped`, `expired`, or `finished` when
  /// the tool call ended without this app answering.
  String? outcome;

  /// Names this app sent a value for (only the names).
  List<String> saved = const [];

  /// This device can't answer (no owner scope): show the card read-only.
  bool readOnly = false;

  /// The tool result, name → `set` / `already_set` / `skipped` / `timed_out`.
  Map<String, String> result = const {};

  bool get open => outcome == null;
}

/// A run that ended other than by completing.
class RunEndItem extends Item {
  RunEndItem({required this.runId, required this.status, this.error});
  final String runId;
  final String status;
  final String? error;
}

/// What a run is busy with: waiting on the model, streaming a reply, or
/// running a tool.
class RunPhase {
  const RunPhase.thinking() : tool = null, writing = false, agents = null;
  const RunPhase.writing() : tool = null, writing = true, agents = null;
  const RunPhase.tool(String this.tool) : writing = false, agents = null;
  const RunPhase.waiting(int running, int done)
    : agents = (running: running, done: done),
      tool = null,
      writing = false;

  final String? tool;
  final bool writing;

  /// Set while the run waits on its sub-agents.
  final ({int running, int done})? agents;

  String get label => switch ((tool, agents)) {
    (final String t, _) => 'Running $t',
    (_, (:final running, :final done)) =>
      'Waiting on $running agent${running == 1 ? '' : 's'}'
          '${done > 0 ? ' · $done done' : ''}',
    _ => writing ? 'Writing' : 'Thinking',
  };
}

/// Parent-side sub-agent tools: the agents card stands in for them.
const _agentTools = {
  'agent_spawn',
  'agent_wait',
  'agent_send',
  'agent_cancel',
  'agent_status',
};

class Transcript {
  final items = <Item>[];

  /// The run in progress here, if any.
  String? get activeRun => _activeRun;
  set activeRun(String? run) {
    if (run != _activeRun) runSince = run == null ? null : DateTime.now();
    _activeRun = run;
  }

  String? _activeRun;

  /// When this app first saw [activeRun] (not necessarily when it began:
  /// the daemon doesn't say).
  DateTime? runSince;

  /// What the run in progress is doing, for the activity line. Null when
  /// nothing runs, or the run is waiting on an approval (its card says so).
  RunPhase? get phase {
    if (activeRun == null) return null;
    final last = items.lastWhere(
      (i) => i is! PillItem,
      orElse: () => UserItem(''),
    );
    final phase = switch (last) {
      ApprovalItem(outcome: null) => null,
      SecretRequestItem(outcome: null) => null,
      WorkItem(:final steps) when last.running => RunPhase.tool(
        steps.lastWhere((s) => s.status == StepStatus.running).name,
      ),
      AssistantItem(streaming: true) => const RunPhase.writing(),
      _ => const RunPhase.thinking(),
    };
    // Between model calls with sub-agents out, the run is waiting on them
    // (an approval card, though, is what it's really waiting on).
    if (phase != null && phase.tool == null && !phase.writing) {
      final kids = [
        for (final a in items.whereType<AgentsItem>())
          if (a.runId == null || a.runId == activeRun) ...a.children,
      ];
      final running = kids.where((k) => k.active).length;
      if (running > 0) {
        return RunPhase.waiting(running, kids.length - running);
      }
    }
    return phase;
  }

  /// Rebuild from stored history (the daemon's view, after compaction
  /// summaries and tool results).
  static Transcript fromSession(SessionDetail s) {
    final t = Transcript();
    final b = _Builder(t);
    for (final m in s.messages) {
      b.add(m);
    }
    return t;
  }

  /// Rebuild a main channel's stream (oldest first): its segments'
  /// messages with a divider at each cut, and posts and side chats where
  /// they happened.
  static Transcript fromStream(Iterable<StreamItem> oldestFirst) {
    final t = Transcript();
    final b = _Builder(t);
    for (final i in oldestFirst) {
      switch (i) {
        case StreamMessage(:final message, :final messageId, :final queued):
          b.add(message, messageId: messageId, queued: queued);
        case StreamCut():
          t.items.add(CutItem(i));
        case StreamPost(:final post):
          t.items.add(PostItem(post));
        case StreamSideChat():
          t.items.add(SideChatItem(i));
        case StreamUnknown():
          break;
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
      case ToolStarted(:final id, :final name, :final summary)
          when _agentTools.contains(name):
        if (_agentCalls.containsKey(id)) return false;
        _agentCalls[id] = name;
        if (name == 'agent_spawn' && _child(id) == null) {
          final label = summary.replaceFirst('agent_spawn', '').trim();
          _agents(e.runId).children.add(
            AgentChild(
              callId: id,
              label: label.isEmpty ? 'agent' : label.split(' ').first,
            )..started = now,
          );
        }
      case ToolFinished(:final id, :final name, :final isError, :final output)
          when _agentTools.contains(name):
        _agentCalls.remove(id);
        _agentResult(name, id, output, isError, now: now);
      case SubagentSpawned(
        :final callId,
        :final childRunId,
        :final childSessionId,
        :final label,
        :final task,
        :final model,
        :final status,
        :final agent,
      ):
        var child = _child(callId);
        if (child == null) {
          child = AgentChild(callId: callId, label: label)..started = now;
          _agents(e.runId).children.add(child);
        }
        child
          ..label = label
          ..task = task
          ..runId = childRunId
          ..sessionId = childSessionId
          ..model = model
          ..status = status
          ..agent = agent ?? child.agent;
      case SubagentMessage(:final kind, :final text, :final runId):
        // A question from one of ours.
        final child = children.where((c) => c.runId == runId).firstOrNull;
        if (child == null || kind != 'question') return false;
        child
          ..status = 'question'
          ..question = text;
      case ToolStarted(:final id, :final name, :final summary):
        if (_step(id) != null) return false;
        final work = _work(e.runId);
        work.started ??= now;
        work.finished = null;
        work.steps.add(
          ToolStep(id: id, name: name, summary: summary)..started = now,
        );
      case ToolOutput(:final id, :final chunk):
        final step = _step(id);
        if (step == null || step.status != StepStatus.running) return false;
        step.output += chunk;
        // Live output is for watching; the full result comes when it ends.
        if (step.output.length > _liveCap) {
          step.output = step.output.substring(step.output.length - _liveCap);
        }
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
        closeSecretRequests(e.runId, id, output);
      case ApprovalRequested(:final approval):
        if (items.any(
          (i) => i is ApprovalItem && i.approval.id == approval.id,
        )) {
          return false;
        }
        items.add(ApprovalItem(approval));
      case SecretRequested(:final request):
        if (items.any(
          (i) => i is SecretRequestItem && i.request.id == request.id,
        )) {
          return false;
        }
        items.add(SecretRequestItem(request));
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
        _agentCalls.clear();
        expireApprovals(runId);
        expireSecretRequests(runId);
        for (final i in items) {
          if (i is AssistantItem) i.streaming = false;
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
      case UsageEvent() ||
          UnknownEvent() ||
          ProvidersChanged() ||
          ModelsChanged() ||
          McpChanged() ||
          PostCreated() ||
          ChannelsChanged() ||
          AgentsChanged() ||
          OutboxChanged() ||
          AuthPromptOpened() ||
          AuthPromptClosed() ||
          ConnectionsChanged():
        return false;
    }
    return true;
  }

  /// Close [run]'s unanswered approvals: it ended without them.
  void expireApprovals(String run) {
    for (final a in items.whereType<ApprovalItem>()) {
      if (a.outcome == null && a.approval.runId == run) a.outcome = 'expired';
    }
  }

  /// Close [run]'s open secret requests: it ended without them.
  void expireSecretRequests(String run) {
    for (final r in items.whereType<SecretRequestItem>()) {
      if (r.open && r.request.runId == run) r.outcome = 'expired';
    }
  }

  /// The `secret_request` call [callId] finished: whoever answered, the
  /// request is over. [output] is `{"NAME":"set"}`, safe to show.
  /// Returns whether a card closed.
  bool closeSecretRequests(String run, String callId, String output) {
    var closed = false;
    for (final r in items.whereType<SecretRequestItem>()) {
      if (r.request.runId != run || r.request.callId != callId) continue;
      if (r.open) {
        r.outcome = 'finished';
        closed = true;
      }
      try {
        if (jsonDecode(output) case final Map m) {
          r.result = {for (final e in m.entries) '${e.key}': '${e.value}'};
        }
      } on FormatException {
        // Not the JSON result (an error message): nothing to show.
      }
    }
    return closed;
  }

  /// Sub-agent tool calls in flight, id → name.
  final _agentCalls = <String, String>{};

  /// Every sub-agent this conversation has started.
  Iterable<AgentChild> get children =>
      items.whereType<AgentsItem>().expand((a) => a.children);

  /// Update a sub-agent from outside: its own run's events, or the
  /// daemon's list. Returns false if it isn't one of ours.
  bool updateChild(
    String runId, {
    String? status,
    String? result,
    String? error,
    DateTime? finished,
  }) {
    final child = children.where((c) => c.runId == runId).firstOrNull;
    if (child == null) return false;
    if (status != null) child.status = status;
    if (status != 'question') child.question = null;
    child
      ..result = result ?? child.result
      ..error = error ?? child.error
      ..finished = finished ?? child.finished;
    return true;
  }

  /// Fill in sub-agents from the daemon's list (after a reload, the history
  /// knows only the calls).
  void mergeChildren(List<ChildRun> runs) {
    for (final r in runs) {
      final child =
          _child(r.callId) ??
          children.where((c) => c.runId == r.runId).firstOrNull;
      if (child == null) continue;
      child
        ..label = r.label
        ..runId = r.runId
        ..sessionId = r.sessionId
        ..model = r.model
        ..status = r.status
        ..error = r.error
        ..question = r.pendingQuestion
        ..started = r.startedAt
        ..finished = r.finishedAt
        ..result = child.result ?? r.resultPreview
        ..agent = r.agentId ?? child.agent;
      if (child.task.isEmpty) child.task = r.task;
    }
  }

  AgentChild? _child(String callId) =>
      children.where((c) => c.callId == callId).firstOrNull;

  /// The agents card to add to: the last item, notes aside.
  AgentsItem _agents([String? runId]) {
    for (final i in items.reversed) {
      if (i is AgentsItem) return i..runId ??= runId;
      if (i is! PillItem) break;
    }
    final a = AgentsItem(runId: runId);
    items.add(a);
    return a;
  }

  /// What a sub-agent tool's result says about the children.
  void _agentResult(
    String name,
    String callId,
    String output,
    bool isError, {
    DateTime? now,
  }) {
    Object? json;
    try {
      json = jsonDecode(output);
    } on FormatException {
      json = null;
    }
    switch ((name, json)) {
      case ('agent_spawn', _) when isError:
        _child(callId)
          ?..status = 'failed'
          ..error = output
          ..finished = now;
      case ('agent_spawn', {'run_id': final String run}):
        final child = _child(callId);
        if (child == null) return;
        child.runId ??= run;
        if (json case {'status': final String st}
            when child.status == 'starting') {
          child.status = st;
        }
        if (json case {'model': final String model}) child.model ??= model;
      case ('agent_wait' || 'agent_status', {'agents': final List agents}):
        for (final a in agents.whereType<Map>()) {
          final child = children
              .where((c) => c.runId == a['run_id'] || c.label == a['agent'])
              .lastOrNull;
          if (child == null) continue;
          if (a['status'] case final String st) child.status = st;
          if (a['result'] case final String r) child.result = r;
          if (a['question'] case final String q) child.question = q;
          if (!child.active) child.finished ??= now;
        }
    }
  }

  static const _liveCap = 64 * 1024;

  /// The output so far of tool call [id], while it runs.
  String? liveOutput(String id) {
    final step = _step(id);
    return step?.status == StepStatus.running ? step!.output : null;
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

/// Turns stored messages into items, pairing tool calls with their results
/// across messages.
class _Builder {
  _Builder(this.t);
  final Transcript t;
  final steps = <String, ToolStep>{};
  final agentCalls = <String, String>{};

  void add(SessionMessage m, {int? messageId, bool queued = false}) {
    if (m.summary) {
      t.items.add(PillItem(PillKind.compacted, 'Earlier messages summarized'));
      return;
    }
    for (final b in m.content) {
      switch (b) {
        // A `channel` step: the agent caught up on the channel's posts.
        case TextBlock(:final text)
            when m.role == 'user' && text.startsWith('[Posts in '):
          final place = text.substring(10).split(' since ').first;
          t.items.add(PillItem(PillKind.posts, 'Caught up on posts in $place'));
        case TextBlock(:final text) when text.trim().isNotEmpty:
          t.items.add(
            m.role == 'user'
                ? UserItem(text, messageId: messageId, queued: queued)
                : AssistantItem(text, messageId: messageId),
          );
        case TextBlock():
          break;
        case ToolUseBlock(:final id, :final name, :final input)
            when _agentTools.contains(name):
          agentCalls[id] = name;
          if (name == 'agent_spawn' && input is Map) {
            t
                ._agents(m.runId)
                .children
                .add(
                  AgentChild(
                    callId: id,
                    label: input['label'] as String? ?? 'agent',
                    task: input['task'] as String? ?? '',
                  )..agent = input['agent'] as String?,
                );
          }
        case ToolResultBlock(:final toolUseId, :final content, :final isError)
            when agentCalls.containsKey(toolUseId):
          t._agentResult(agentCalls[toolUseId]!, toolUseId, content, isError);
        case ToolUseBlock(:final id, :final name, :final input):
          final step = ToolStep(
            id: id,
            name: name,
            summary: summarize(name, input),
          );
          steps[id] = step;
          t._work(m.runId).steps.add(step);
        case ToolResultBlock(:final toolUseId, :final content, :final isError):
          final step = steps[toolUseId];
          if (step == null) break;
          step
            ..status = isError ? StepStatus.error : StepStatus.ok
            ..output = content;
          if (!isError) t._pillFor(step.name, step.summary);
      }
    }
  }
}
