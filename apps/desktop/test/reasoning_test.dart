import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/items.dart';
import 'package:tern_desktop/ui/reasoning.dart';

Widget _host(Widget child, {Brightness brightness = Brightness.light}) {
  final c = brightness == Brightness.light
      ? TernColors.light
      : TernColors.dark;
  return MaterialApp(
    theme: ternTheme(c, brightness),
    home: Scaffold(body: Center(child: SizedBox(width: 500, child: child))),
  );
}

const _chevron = ValueKey('reasoning-chevron');
const _box = ValueKey('reasoning-box');
const _toggle = ValueKey('reasoning-toggle');

void main() {
  group('Transcript reasoning', () {
    test('streams in, ends when the answer starts, and stays on the run', () {
      final t = Transcript();
      final t0 = DateTime(2026, 11, 6, 9);
      t.apply(const RunStarted(runId: 'r', sessionId: 's'), now: t0);
      t.apply(
        const ReasoningDelta(runId: 'r', text: 'Let me '),
        now: t0,
      );
      t.apply(
        const ReasoningDelta(runId: 'r', text: 'think.'),
        now: t0.add(const Duration(seconds: 4)),
      );
      expect(t.reasoningFor('r')!.text, 'Let me think.');
      expect(t.reasoningFor('r')!.live, isTrue);
      t.apply(
        const TextDelta(runId: 'r', text: 'Done.'),
        now: t0.add(const Duration(seconds: 12)),
      );
      final r = t.reasoningFor('r')!;
      expect(r.live, isFalse);
      expect(r.duration(), const Duration(seconds: 12));
      // The reasoning sits before the answer it led to.
      expect(t.items.first, isA<ReasoningItem>());
      expect(t.items.last, isA<AssistantItem>());
    });

    test('a run with no reasoning has none', () {
      final t = Transcript();
      t.apply(const RunStarted(runId: 'r', sessionId: 's'));
      t.apply(const TextDelta(runId: 'r', text: 'Hi'));
      expect(t.items.whereType<ReasoningItem>(), isEmpty);
      expect(t.reasoningFor('r'), isNull);
    });

    test('redacted reasoning is present but has no text', () {
      final t = Transcript();
      t.apply(const RunStarted(runId: 'r', sessionId: 's'));
      t.apply(const ReasoningHidden(runId: 'r'));
      final r = t.reasoningFor('r')!;
      expect(r.hidden, isTrue);
      expect(r.text, isEmpty);
    });

    test('model calls of one run join, in order', () {
      final t = Transcript();
      final t0 = DateTime(2026, 11, 6, 9);
      t.apply(const RunStarted(runId: 'r', sessionId: 's'), now: t0);
      t.apply(const ReasoningDelta(runId: 'r', text: 'First.'), now: t0);
      t.apply(
        const ToolStarted(runId: 'r', id: 'c', name: 'shell', summary: 'ls'),
        now: t0.add(const Duration(seconds: 2)),
      );
      t.apply(
        const ReasoningDelta(runId: 'r', text: 'Second.'),
        now: t0.add(const Duration(seconds: 5)),
      );
      t.apply(
        const TextDelta(runId: 'r', text: 'ok'),
        now: t0.add(const Duration(seconds: 8)),
      );
      final r = t.reasoningFor('r')!;
      expect(r.text, 'First.\n\nSecond.');
      expect(r.duration(), const Duration(seconds: 5));
    });

    test('rebuilds from stored blocks after a reload', () {
      final t = Transcript.fromSession(
        const SessionDetail(
          id: 's',
          messages: [
            SessionMessage(
              role: 'assistant',
              runId: 'r',
              summary: false,
              content: [
                ReasoningBlock(text: 'Weighed it up.', durationMs: 12400),
                TextBlock('Answer.'),
              ],
            ),
          ],
        ),
      );
      final r = t.reasoningFor('r')!;
      expect(r.text, 'Weighed it up.');
      expect(r.live, isFalse);
      expect(thoughtLabel(r), 'Thought for 12s');
    });

    test('parses the wire shapes', () {
      final a = ContentBlock.fromJson({
        'type': 'thinking',
        'thinking': 'hm',
        'duration_ms': 900,
      });
      expect((a as ReasoningBlock).text, 'hm');
      final b = ContentBlock.fromJson({'type': 'redacted_thinking', 'data': 'x'});
      expect((b as ReasoningBlock).hidden, isTrue);
      expect(
        TernEvent.fromJson({
          'type': 'reasoning_delta',
          'run_id': 'r',
          'text': 'a',
        }),
        isA<ReasoningDelta>(),
      );
      expect(
        TernEvent.fromJson({'type': 'reasoning_hidden', 'run_id': 'r'}),
        isA<ReasoningHidden>(),
      );
    });
  });

  group('ReasoningNote', () {
    final done = Reasoning(
      text: 'Weighed the options.',
      elapsed: const Duration(seconds: 12),
    );

    for (final b in Brightness.values) {
      testWidgets('is collapsed, then opens and closes (${b.name})', (
        tester,
      ) async {
        await tester.pumpWidget(
          _host(ReasoningNote(reasoning: done), brightness: b),
        );
        expect(find.text('Thought for 12s'), findsOneWidget);
        expect(find.byKey(_box), findsNothing);

        await tester.tap(find.byKey(_toggle));
        await tester.pump();
        expect(find.byKey(_box), findsOneWidget);
        expect(find.text('Weighed the options.'), findsOneWidget);
        // One step off the page, no border.
        final decoration =
            tester.widget<Container>(find.byKey(_box)).decoration!
                as BoxDecoration;
        final c = b == Brightness.light ? TernColors.light : TernColors.dark;
        expect(decoration.color, c.sidebar);
        expect(decoration.border, isNull);
        expect(decoration.boxShadow, isNull);

        await tester.tap(find.byKey(_toggle));
        await tester.pump();
        expect(find.byKey(_box), findsNothing);
      });
    }

    testWidgets('shows the hidden message when the provider withheld it', (
      tester,
    ) async {
      await tester.pumpWidget(
        _host(ReasoningNote(reasoning: Reasoning(hidden: true))),
      );
      await tester.tap(find.byKey(_toggle));
      await tester.pump();
      expect(find.text('Reasoning hidden'), findsOneWidget);
    });
  });

  group('ActivityLine', () {
    testWidgets('shows no chevron when there is no reasoning', (tester) async {
      await tester.pumpWidget(
        _host(const ActivityLine(phase: RunPhase.thinking())),
      );
      expect(find.text('Thinking…'), findsOneWidget);
      expect(find.byKey(_chevron), findsNothing);
      expect(find.byKey(_toggle), findsNothing);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('opens a box that follows the streaming text', (tester) async {
      final r = Reasoning()..add('Starting. ', DateTime(2026));
      Widget line() =>
          _host(ActivityLine(phase: const RunPhase.thinking(), reasoning: r));
      await tester.pumpWidget(line());
      // Collapsed by default, with a chevron.
      expect(find.byKey(_chevron), findsOneWidget);
      expect(find.byKey(_box), findsNothing);

      await tester.tap(find.byKey(_toggle));
      await tester.pump();
      expect(find.byKey(_box), findsOneWidget);
      expect(find.text('Starting. '), findsOneWidget);

      // More arrives while it is open: the box shows it.
      r.add('And then more.', DateTime(2026));
      await tester.pumpWidget(line());
      expect(find.text('Starting. And then more.'), findsOneWidget);

      // Plenty of text: it caps its height and keeps the end in view.
      r.add(List.generate(80, (i) => 'line $i').join('\n'), DateTime(2026));
      await tester.pumpWidget(line());
      await tester.pump();
      final box = tester.getSize(find.byKey(_box));
      expect(box.height, ReasoningBox.maxHeight);
      // The box's own scroller (the selectable text has one inside).
      final scroll = tester.state<ScrollableState>(
        find
            .descendant(
              of: find.byKey(_box),
              matching: find.byType(Scrollable),
            )
            .first,
      );
      expect(scroll.position.pixels, scroll.position.maxScrollExtent);

      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('stops following once the reader scrolls up', (tester) async {
      final r = Reasoning()
        ..add(List.generate(80, (i) => 'line $i').join('\n'), DateTime(2026));
      Widget line() =>
          _host(ActivityLine(phase: const RunPhase.thinking(), reasoning: r));
      await tester.pumpWidget(line());
      await tester.tap(find.byKey(_toggle));
      await tester.pump();
      await tester.pump();
      final scrollable = find
          .descendant(of: find.byKey(_box), matching: find.byType(Scrollable))
          .first;
      await tester.drag(scrollable, const Offset(0, 300));
      await tester.pump();
      final before = tester.state<ScrollableState>(scrollable).position.pixels;
      r.add('\nmore', DateTime(2026));
      await tester.pumpWidget(line());
      await tester.pump();
      expect(
        tester.state<ScrollableState>(scrollable).position.pixels,
        before,
      );
      await tester.pumpWidget(const SizedBox());
    });
  });
}
