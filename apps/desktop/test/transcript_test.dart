import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/transcript.dart';

Approval approval(String id) => Approval(
  id: id,
  runId: 'r',
  sessionId: 's',
  tool: 'shell',
  input: const {'command': 'ls'},
  summary: 'shell ls',
  pattern: 'ls',
);

void main() {
  group('sub-agents', subagentTests);

  test('a live run becomes message, work card, message', () {
    final t = Transcript()..items.add(UserItem('hi'));
    final t0 = DateTime(2026, 11, 3, 9);
    for (final e in <TernEvent>[
      const RunStarted(runId: 'r', sessionId: 's'),
      const TextDelta(runId: 'r', text: 'Check'),
      const TextDelta(runId: 'r', text: 'ing.'),
      const AssistantMessage(runId: 'r', text: 'Checking.', replayed: false),
      const ToolStarted(
        runId: 'r',
        id: 't1',
        name: 'shell',
        summary: 'shell ls',
      ),
    ]) {
      t.apply(e, now: t0);
    }
    expect(t.activeRun, 'r');
    expect((t.items[1] as AssistantItem).streaming, isFalse);
    final work = t.items[2] as WorkItem;
    expect(work.running, isTrue);

    t.apply(
      const ToolFinished(
        runId: 'r',
        id: 't1',
        name: 'shell',
        isError: false,
        preview: '',
        output: 'a\nb',
        replayed: false,
      ),
      now: t0.add(const Duration(seconds: 3)),
    );
    t.apply(const TextDelta(runId: 'r', text: 'Done.'));
    t.apply(const AssistantMessage(runId: 'r', text: 'Done.', replayed: false));
    t.apply(const RunFinished(runId: 'r', status: 'completed'));

    expect(work.elapsed, const Duration(seconds: 3));
    expect(work.steps.single.output, 'a\nb');
    expect(t.items.map((i) => i.runtimeType), [
      UserItem,
      AssistantItem,
      WorkItem,
      AssistantItem,
    ]);
    expect(t.activeRun, isNull);
  });

  test('the run phase follows what the run is doing', () {
    final t = Transcript()..items.add(UserItem('hi'));
    expect(t.phase, isNull);
    t.apply(const RunStarted(runId: 'r', sessionId: 's'));
    expect(t.phase?.label, 'Thinking');
    expect(t.runSince, isNotNull);
    t.apply(const TextDelta(runId: 'r', text: 'Hm'));
    expect(t.phase?.label, 'Writing');
    t.apply(const AssistantMessage(runId: 'r', text: 'Hm', replayed: false));
    expect(t.phase?.label, 'Thinking');
    t.apply(
      const ToolStarted(runId: 'r', id: 't1', name: 'shell', summary: 'ls'),
    );
    expect(t.phase?.label, 'Running shell');
    t.apply(ApprovalRequested(approval('a1')));
    expect(t.phase, isNull, reason: 'the approval card speaks for itself');
    t.apply(const RunFinished(runId: 'r', status: 'completed'));
    expect(t.phase, isNull);
    expect(t.runSince, isNull);
  });

  test('live tool output shows while running, then the result replaces it', () {
    final t = Transcript()
      ..apply(const RunStarted(runId: 'r', sessionId: 's'))
      ..apply(
        const ToolStarted(runId: 'r', id: 't1', name: 'shell', summary: 'make'),
      )
      ..apply(const ToolOutput(runId: 'r', id: 't1', chunk: 'one\n'))
      ..apply(const ToolOutput(runId: 'r', id: 't1', chunk: 'two\n'));
    expect(t.liveOutput('t1'), 'one\ntwo\n');
    t.apply(
      const ToolFinished(
        runId: 'r',
        id: 't1',
        name: 'shell',
        isError: false,
        preview: '',
        output: 'exit 0',
        replayed: false,
      ),
    );
    expect(t.liveOutput('t1'), isNull);
    expect((t.items.single as WorkItem).steps.single.output, 'exit 0');
    expect(
      t.apply(const ToolOutput(runId: 'r', id: 't1', chunk: 'late')),
      isFalse,
    );
  });

  test('tool-only turns leave no empty message', () {
    final t = Transcript()
      ..apply(const AssistantMessage(runId: 'r', text: '', replayed: false));
    expect(t.items, isEmpty);
  });

  test('replayed events after a crash are not duplicated', () {
    final t = Transcript()
      ..apply(
        const AssistantMessage(runId: 'r', text: 'Hello', replayed: false),
      )
      ..apply(const ToolStarted(runId: 'r', id: 't1', name: 'x', summary: 'x'));
    expect(
      t.apply(const ToolStarted(runId: 'r', id: 't1', name: 'x', summary: 'x')),
      isFalse,
    );
    t.apply(const AssistantMessage(runId: 'r', text: 'Hello', replayed: true));
    expect(t.items.whereType<AssistantItem>(), hasLength(1));
  });

  test('approvals are shown once and close when answered or the run ends', () {
    final t = Transcript()..apply(ApprovalRequested(approval('a1')));
    expect(t.apply(ApprovalRequested(approval('a1'))), isFalse);
    t.apply(
      const ApprovalAnswered(
        runId: 'r',
        id: 'x',
        name: 'shell',
        outcome: 'denied',
      ),
    );
    expect((t.items.single as ApprovalItem).outcome, 'denied');

    t.apply(ApprovalRequested(approval('a2')));
    t.apply(const RunFinished(runId: 'r', status: 'interrupted'));
    expect((t.items[1] as ApprovalItem).outcome, 'expired');
    expect(t.items.last, isA<RunEndItem>());
  });

  test('history rebuilds steps with results and notes', () {
    final t = Transcript.fromSession(
      const SessionDetail(
        id: 's',
        messages: [
          SessionMessage(
            role: 'user',
            summary: true,
            content: [TextBlock('sum')],
          ),
          SessionMessage(
            role: 'user',
            summary: false,
            content: [TextBlock('save it')],
          ),
          SessionMessage(
            role: 'assistant',
            summary: false,
            content: [
              TextBlock('Saving.'),
              ToolUseBlock(
                id: 't',
                name: 'memory_save',
                input: {'text': 'likes tea'},
              ),
            ],
          ),
          SessionMessage(
            role: 'user',
            summary: false,
            content: [
              ToolResultBlock(toolUseId: 't', content: 'saved', isError: false),
            ],
          ),
          SessionMessage(
            role: 'assistant',
            summary: false,
            content: [TextBlock('Done.')],
          ),
        ],
      ),
    );
    expect(t.items.map((i) => i.runtimeType), [
      PillItem,
      UserItem,
      AssistantItem,
      WorkItem,
      PillItem,
      AssistantItem,
    ]);
    final step = (t.items[3] as WorkItem).steps.single;
    expect(step.summary, 'memory_save likes tea');
    expect(step.status, StepStatus.ok);
  });

  test('summaries prefer the meaningful argument', () {
    expect(
      summarize('shell', {'timeout': 5, 'command': 'ls -la'}),
      'shell ls -la',
    );
    expect(summarize('x', {'n': 1}), 'x {"n":1}');
    expect(summarize('x', null), 'x');
  });

  test('a note after a tool does not start a new card', () {
    final t = Transcript();
    for (final e in <TernEvent>[
      const ToolStarted(runId: 'r', id: '1', name: 'memory_save', summary: 'm'),
      const ToolFinished(
        runId: 'r',
        id: '1',
        name: 'memory_save',
        isError: false,
        preview: '',
        output: '',
        replayed: false,
      ),
      const ToolStarted(runId: 'r', id: '2', name: 'shell', summary: 's'),
    ]) {
      t.apply(e);
    }
    expect(t.items.map((i) => i.runtimeType), [WorkItem, PillItem]);
    expect((t.items.first as WorkItem).steps, hasLength(2));
  });
}

void subagentTests() {
  ToolStarted spawn(String id, String label) => ToolStarted(
    runId: 'p',
    id: id,
    name: 'agent_spawn',
    summary: 'agent_spawn $label',
  );
  SubagentSpawned spawned(String id, String label) => SubagentSpawned(
    runId: 'p',
    callId: id,
    childRunId: 'run-$label',
    childSessionId: 'sess-$label',
    label: label,
    task: 'Look into $label',
    model: 'mock:fast',
    status: 'running',
  );

  test('a fan-out becomes one agents card, not tool steps', () {
    final t = Transcript()..items.add(UserItem('compare a and b'));
    for (final e in <TernEvent>[
      const RunStarted(runId: 'p', sessionId: 's'),
      spawn('c1', 'a'),
      spawned('c1', 'a'),
      const ToolFinished(
        runId: 'p',
        id: 'c1',
        name: 'agent_spawn',
        isError: false,
        preview: '',
        output: '{"agent":"a","run_id":"run-a","status":"running"}',
        replayed: false,
      ),
      spawn('c2', 'b'),
      spawned('c2', 'b'),
      const ToolStarted(
        runId: 'p',
        id: 'w',
        name: 'agent_wait',
        summary: 'agent_wait',
      ),
    ]) {
      t.apply(e);
    }
    expect(t.items.map((i) => i.runtimeType), [UserItem, AgentsItem]);
    final card = t.items.last as AgentsItem;
    expect(card.children.map((c) => c.label), ['a', 'b']);
    expect(card.children.first.sessionId, 'sess-a');
    expect(t.phase?.label, 'Waiting on 2 agents');

    expect(
      t.updateChild('run-a', status: 'completed', result: 'A wins'),
      isTrue,
    );
    expect(t.phase?.label, 'Waiting on 1 agent · 1 done');

    t.apply(
      const SubagentMessage(
        runId: 'run-b',
        toRunId: 'p',
        kind: 'question',
        text: 'Include drafts?',
      ),
    );
    expect(card.children.last.status, 'question');

    t.apply(ApprovalRequested(approval('a1')));
    expect(t.phase, isNull, reason: 'the approval card speaks for itself');
    t.apply(
      const ApprovalAnswered(
        runId: 'r',
        id: 'a1',
        name: 'shell',
        outcome: 'approved',
      ),
    );
    expect(t.phase?.label, 'Waiting on 1 agent · 1 done');

    t.apply(
      const ToolFinished(
        runId: 'p',
        id: 'w',
        name: 'agent_wait',
        isError: false,
        preview: '',
        output: '{"agents":[{"agent":"b","run_id":"run-b","status":"completed","result":"B"}]}',
        replayed: false,
      ),
    );
    expect(card.children.last.status, 'completed');
    expect(card.children.last.result, 'B');
    expect(t.phase?.label, 'Thinking');
  });

  test('a failed spawn shows on the card', () {
    final t = Transcript()
      ..apply(const RunStarted(runId: 'p', sessionId: 's'))
      ..apply(spawn('c1', 'a'))
      ..apply(
        const ToolFinished(
          runId: 'p',
          id: 'c1',
          name: 'agent_spawn',
          isError: true,
          preview: '',
          output: 'label a is taken',
          replayed: false,
        ),
      );
    final child = (t.items.single as AgentsItem).children.single;
    expect(child.status, 'failed');
    expect(child.error, 'label a is taken');
  });

  test('history rebuilds the card, and the daemon list fills it in', () {
    final t = Transcript.fromSession(
      const SessionDetail(
        id: 's',
        messages: [
          SessionMessage(
            role: 'assistant',
            summary: false,
            runId: 'p',
            content: [
              ToolUseBlock(
                id: 'c1',
                name: 'agent_spawn',
                input: {'label': 'a', 'task': 'Look into a'},
              ),
              ToolUseBlock(id: 'w', name: 'agent_wait', input: {}),
            ],
          ),
          SessionMessage(
            role: 'user',
            summary: false,
            content: [
              ToolResultBlock(
                toolUseId: 'c1',
                content: '{"agent":"a","run_id":"run-a","status":"running"}',
                isError: false,
              ),
              ToolResultBlock(
                toolUseId: 'w',
                content: '{"agents":[{"agent":"a","run_id":"run-a","status":"completed","result":"A"}]}',
                isError: false,
              ),
            ],
          ),
        ],
      ),
    );
    final child = (t.items.single as AgentsItem).children.single;
    expect(child.runId, 'run-a');
    expect(child.status, 'completed');
    expect(child.task, 'Look into a');

    t.mergeChildren([
      ChildRun(
        runId: 'run-a',
        sessionId: 'sess-a',
        parentRunId: 'p',
        callId: 'c1',
        label: 'a',
        task: 'Look into a',
        model: 'mock:fast',
        depth: 1,
        status: 'completed',
        startedAt: DateTime(2026),
        finishedAt: DateTime(2026, 1, 1, 0, 1),
        usage: const Usage(),
      ),
    ]);
    expect(child.sessionId, 'sess-a');
    expect(child.model, 'mock:fast');
    expect(child.result, 'A', reason: 'the full report beats the preview');
  });
}
