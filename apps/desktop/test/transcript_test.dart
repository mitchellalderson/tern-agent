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
