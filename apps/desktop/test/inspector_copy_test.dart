// The run inspector's copy buttons put a failed connector call's details on
// the clipboard, so the error can be pasted somewhere.
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/inspector.dart';

const _error =
    'http: host "api.weather.com" is not in this connector\'s '
    'capabilities.http (allowed: weather.example.com)';

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  @override
  Future<List<RunItem>> runs(String session, {bool subagents = false}) async =>
      [
        RunItem(
          id: 'run_1',
          status: 'failed',
          trigger: 'user',
          model: 'anthropic:claude-sonnet-5-5',
          startedAt: DateTime(2026, 11, 3, 9),
          error: 'tool weather__forecast failed',
        ),
      ];

  @override
  Future<List<StepItem>> steps(String run) async => [
    StepItem(
      idx: 0,
      kind: 'tool',
      status: 'failed',
      input: {
        'id': 'call_1',
        'name': 'weather__forecast',
        'input': {'city': 'Oslo'},
      },
      error: _error,
      startedAt: DateTime(2026, 11, 3, 9),
      finishedAt: DateTime(2026, 11, 3, 9, 0, 1),
    ),
  ];
}

void main() {
  testWidgets('copy buttons put run and step details on the clipboard', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1000, 700) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);

    String? clipboard;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          clipboard = (call.arguments as Map)['text'] as String;
        }
        return null;
      },
    );

    final store = TernStore(_FakeClient());
    await tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: Scaffold(
          body: RunInspector(store: store, session: 's1'),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // The whole run: summary, run error, the step list and the failed
    // step's input and error.
    await tester.tap(find.byKey(const Key('copy-run')));
    await tester.pump();
    expect(clipboard, contains('Run run_1'));
    expect(clipboard, contains('Error: tool weather__forecast failed'));
    expect(clipboard, contains('"city": "Oslo"'));
    expect(clipboard, contains(_error));
    expect(find.byTooltip('Copied'), findsOneWidget);

    // Just the step's error, from its block.
    await tester.tap(find.text(_label(tester)).first);
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('copy-0-ERROR')));
    await tester.pump();
    expect(clipboard, _error);

    // The check goes back to a copy icon.
    await tester.pump(const Duration(seconds: 2));
    expect(find.byTooltip('Copied'), findsNothing);
  });
}

/// The failed step's row label (failed steps aren't expanded by default).
String _label(WidgetTester tester) => tester
    .widgetList<Text>(find.byType(Text))
    .map((t) => t.data ?? '')
    .firstWhere((t) => t.contains('weather'));
