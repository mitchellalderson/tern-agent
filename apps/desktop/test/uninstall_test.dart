// Settings › About › Uninstall tern… against a fake `tern` command.
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/state/daemon.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/uninstall.dart';

Future<void> _open(
  WidgetTester tester, {
  required List<List<String>> runs,
  required List<String> revealed,
  required List<String> events,
  int exitCode = 0,
  String stderr = '',
}) async {
  final control = TernControl(
    bin: '/fake/tern',
    env: const {'HOME': '/nonexistent'},
    run: (bin, args) async {
      runs.add(args);
      return ProcessResult(0, exitCode, '', stderr);
    },
  );
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: Builder(
          builder: (context) => TextButton(
            onPressed: () => showUninstallDialog(
              context,
              control: control,
              dataDir: '/Users/you/.tern',
              reveal: (p) async => revealed.add(p),
              exitApp: () async => events.add('exit'),
              macOS: true,
            ),
            child: const Text('open'),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('open'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('keeps data by default and runs `uninstall`', (tester) async {
    final runs = <List<String>>[];
    final revealed = <String>[];
    final events = <String>[];
    await _open(tester, runs: runs, revealed: revealed, events: events);
    expect(find.text('Also delete my data'), findsOneWidget);
    expect(find.byKey(const Key('uninstall-path')), findsNothing);
    expect(find.byKey(const Key('uninstall-warning')), findsNothing);

    await tester.tap(find.byKey(const Key('uninstall-confirm')));
    await tester.pumpAndSettle();
    expect(runs, [
      ['uninstall'],
    ]);
    expect(
      find.text(
        'tern is stopped and removed from login items. '
        'Drag tern to the Trash to finish.',
      ),
      findsOneWidget,
    );
    expect(events, isEmpty);

    await tester.tap(find.byKey(const Key('uninstall-finder')));
    await tester.pumpAndSettle();
    expect(revealed, ['/Applications/tern.app']);
    expect(events, ['exit']);
  });

  testWidgets('with the checkbox, shows the folder and purges', (tester) async {
    final runs = <List<String>>[];
    await _open(tester, runs: runs, revealed: [], events: []);
    await tester.tap(find.byKey(const Key('uninstall-purge')));
    await tester.pumpAndSettle();
    expect(find.text('/Users/you/.tern'), findsOneWidget);
    expect(
      find.text('Includes API keys, agents and memory. This can’t be undone.'),
      findsOneWidget,
    );

    await tester.tap(find.byKey(const Key('uninstall-confirm')));
    await tester.pumpAndSettle();
    expect(runs, [
      ['uninstall', '--purge', '--yes'],
    ]);
  });

  testWidgets('cancel runs nothing', (tester) async {
    final runs = <List<String>>[];
    await _open(tester, runs: runs, revealed: [], events: []);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(runs, isEmpty);
    expect(find.text('Uninstall tern?'), findsNothing);
  });

  testWidgets('a failure is shown and the app stays open', (tester) async {
    final events = <String>[];
    await _open(
      tester,
      runs: [],
      revealed: [],
      events: events,
      exitCode: 1,
      stderr: 'could not stop tern',
    );
    await tester.tap(find.byKey(const Key('uninstall-confirm')));
    await tester.pumpAndSettle();
    expect(find.text('could not stop tern'), findsOneWidget);
    expect(find.byKey(const Key('uninstall-confirm')), findsOneWidget);
    expect(events, isEmpty);
  });
}
