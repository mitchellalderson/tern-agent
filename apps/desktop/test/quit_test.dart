// "Quit tern completely" and Settings › General (personal-agent-tooling-spec
// §11.5a) against a fake client and a fake `tern` command.
import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/daemon.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/background_prompt.dart';
import 'package:tern_desktop/ui/quit.dart';
import 'package:tern_desktop/ui/settings.dart';

class _FakeClient extends TernClient {
  _FakeClient({this.running = const [], this.stops = true})
    : super(const Endpoint.unix('/nonexistent'));

  final List<PausedRun> running;

  /// Whether tern goes away when asked.
  final bool stops;
  final calls = <String>[];
  var _stopped = false;

  @override
  Future<ShutdownReply> shutdown({bool dryRun = false}) async {
    calls.add(dryRun ? 'dry_run' : 'shutdown');
    if (!dryRun && stops) _stopped = true;
    return ShutdownReply(stopping: !dryRun, runs: running);
  }

  @override
  Future<Health> health() async {
    if (_stopped) throw const TernException('connection refused');
    return const Health(pid: 1, version: '0.1.0');
  }
}

/// A `tern` binary that records its arguments and succeeds.
TernControl _control(List<List<String>> runs, {Map<String, String>? env}) =>
    TernControl(
      bin: '/fake/tern',
      env: env ?? const {'HOME': '/nonexistent'},
      run: (bin, args) async {
        runs.add(args);
        return ProcessResult(0, 0, '', '');
      },
    );

Widget _app(Widget Function(BuildContext) child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: Builder(builder: child)),
);

void main() {
  const report = PausedRun(
    runId: 'r1',
    sessionId: 's1',
    title: 'Summarize the Q3 report',
  );

  TernStore store(_FakeClient client) =>
      TernStore(client)..health = const Health(pid: 1, version: '0.1.0');

  test('the message names the tasks and says they resume', () {
    expect(
      quitMessage([report]),
      '1 task is in progress ("Summarize the Q3 report"). It will pick up '
      'where it left off next time tern starts.\n'
      'Routines and Telegram will be paused until then.',
    );
    expect(
      quitMessage([
        report,
        const PausedRun(runId: 'r2', sessionId: 's2', title: 'Morning brief'),
      ]),
      startsWith(
        '2 tasks are in progress ("Summarize the Q3 report", '
        '"Morning brief"). They will pick up where they left off',
      ),
    );
  });

  testWidgets('a task in progress: confirm names it, then tern stops', (
    tester,
  ) async {
    final client = _FakeClient(running: [report]);
    final runs = <List<String>>[];
    var exited = false;
    late BuildContext ctx;
    await tester.pumpWidget(
      _app((c) {
        ctx = c;
        return const SizedBox();
      }),
    );
    final done = quitTernCompletely(
      ctx,
      store(client),
      _control(runs),
      exitApp: () async => exited = true,
    );
    await tester.pumpAndSettle();
    expect(find.text('Quit tern?'), findsOneWidget);
    expect(find.textContaining('"Summarize the Q3 report"'), findsOneWidget);
    expect(find.textContaining('pick up where it left off'), findsOneWidget);
    expect(client.calls, ['dry_run']);

    await tester.tap(find.widgetWithText(FilledButton, 'Quit tern'));
    await tester.pumpAndSettle();
    expect(await done, isTrue);
    expect(client.calls, ['dry_run', 'shutdown']);
    expect(exited, isTrue);
    expect(runs, isEmpty, reason: 'it stopped when asked: no `tern stop`');
  });

  testWidgets('cancel leaves tern and the app running', (tester) async {
    final client = _FakeClient(running: [report]);
    var exited = false;
    late BuildContext ctx;
    await tester.pumpWidget(
      _app((c) {
        ctx = c;
        return const SizedBox();
      }),
    );
    final done = quitTernCompletely(
      ctx,
      store(client),
      _control([]),
      exitApp: () async => exited = true,
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(await done, isFalse);
    expect(client.calls, ['dry_run']);
    expect(exited, isFalse);
  });

  testWidgets('nothing running: no question asked', (tester) async {
    final client = _FakeClient();
    var exited = false;
    late BuildContext ctx;
    await tester.pumpWidget(
      _app((c) {
        ctx = c;
        return const SizedBox();
      }),
    );
    final done = quitTernCompletely(
      ctx,
      store(client),
      _control([]),
      exitApp: () async => exited = true,
    );
    await tester.pumpAndSettle();
    expect(find.text('Quit tern?'), findsNothing);
    expect(await done, isTrue);
    expect(client.calls, ['dry_run', 'shutdown']);
    expect(exited, isTrue);
  });

  test('a tern that ignores shutdown is stopped with `tern stop`', () async {
    final client = _FakeClient(stops: false);
    final runs = <List<String>>[];
    await stopTern(
      client,
      _control(runs),
      wait: const Duration(milliseconds: 300),
    );
    expect(client.calls, ['shutdown']);
    expect(runs, [
      ['stop'],
    ]);
  });

  testWidgets('settings: start at login and what closing does', (tester) async {
    final dir = Directory.systemTemp.createTempSync('tern_settings');
    addTearDown(() => dir.deleteSync(recursive: true));
    final runs = <List<String>>[];
    final control = _control(runs, env: {'HOME': dir.path});
    final file = File('${dir.path}/prefs/desktop.json');
    final prefs = AppPrefs.fromFile(file);
    expect(prefs.onClose, OnClose.keepRunning);

    await tester.pumpWidget(
      _app((_) => SettingsDialog(control: control, prefs: prefs)),
    );
    expect(find.text('Start tern when I log in'), findsOneWidget);
    expect(
      find.text('Keep tern running in the background (recommended)'),
      findsOneWidget,
    );
    expect(find.textContaining('daemon'), findsNothing);

    final toggle = find.byType(Switch);
    expect(tester.widget<Switch>(toggle).value, isFalse);
    await tester.tap(toggle);
    await tester.pumpAndSettle();
    // The dialog asks tern how it starts, before and after each change.
    expect(runs.where((r) => r[1] != 'status'), [
      ['service', 'install', if (Platform.isMacOS) '--tray'],
    ]);
    expect(runs.first, ['service', 'status', '--json']);
    expect(tester.widget<Switch>(toggle).value, isTrue);
    await tester.tap(toggle);
    await tester.pumpAndSettle();
    expect(runs.where((r) => r[1] != 'status').last, ['service', 'uninstall']);
    expect(tester.widget<Switch>(toggle).value, isFalse);

    await tester.runAsync(() async {
      await tester.tap(find.text('Quit tern completely'));
      await tester.pump();
      // Saving is real file IO.
      final end = DateTime.now().add(const Duration(seconds: 5));
      while (!file.existsSync() && DateTime.now().isBefore(end)) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
    });
    expect(prefs.onClose, OnClose.quitTern);
    expect(AppPrefs.fromFile(file).onClose, OnClose.quitTern);
  });

  testWidgets('a failing install shows what tern said', (tester) async {
    final control = TernControl(
      bin: '/fake/tern',
      env: const {'HOME': '/nonexistent'},
      run: (bin, args) async =>
          ProcessResult(0, 1, '', 'error: writing the plist: denied'),
    );
    await tester.pumpWidget(
      _app(
        (_) => SettingsDialog(
          control: control,
          prefs: AppPrefs(File('/nonexistent/desktop.json')),
        ),
      ),
    );
    await tester.tap(find.byType(Switch));
    await tester.pumpAndSettle();
    expect(find.text('error: writing the plist: denied'), findsOneWidget);
    expect(tester.widget<Switch>(find.byType(Switch)).value, isFalse);
  });

  testWidgets('the settings switch follows what tern reports', (tester) async {
    final control = TernControl(
      bin: '/fake/tern',
      env: const {'HOME': '/nonexistent'},
      run: (bin, args) async => ProcessResult(
        0,
        0,
        '{"method":"app_service","at_login":false,"needs_approval":true}',
        '',
      ),
    );
    await tester.pumpWidget(
      _app(
        (_) => SettingsDialog(
          control: control,
          prefs: AppPrefs(File('/nonexistent/desktop.json')),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(tester.widget<Switch>(find.byType(Switch)).value, isTrue);
    expect(find.textContaining('macOS needs your OK'), findsOneWidget);
  });

  testWidgets('first launch asks once to keep tern running', (tester) async {
    final dir = Directory.systemTemp.createTempSync('tern_ask');
    addTearDown(() => dir.deleteSync(recursive: true));
    final runs = <List<String>>[];
    final control = _control(runs, env: {'HOME': dir.path});
    final file = File('${dir.path}/desktop.json');
    final prefs = AppPrefs.fromFile(file);
    expect(prefs.askedBackground, isFalse);

    late BuildContext ctx;
    await tester.pumpWidget(
      _app((c) {
        ctx = c;
        return const SizedBox();
      }),
    );
    unawaited(maybeAskKeepRunning(ctx, control, prefs));
    await tester.pumpAndSettle();
    expect(find.text('Keep tern running in the background?'), findsOneWidget);
    expect(find.textContaining('daemon'), findsNothing);
    await tester.tap(find.text('Start at login'));
    await tester.pumpAndSettle();
    expect(runs.last, ['service', 'install', if (Platform.isMacOS) '--tray']);
    expect(find.text('Keep tern running in the background?'), findsNothing);
    expect(prefs.askedBackground, isTrue);

    // Answered: never asked again, and that's saved.
    runs.clear();
    await maybeAskKeepRunning(ctx, control, prefs);
    await tester.pumpAndSettle();
    expect(find.text('Keep tern running in the background?'), findsNothing);
    expect(runs, isEmpty);
  });

  testWidgets('first launch doesn\'t ask when tern already starts at login', (
    tester,
  ) async {
    final control = TernControl(
      bin: '/fake/tern',
      env: const {'HOME': '/nonexistent'},
      run: (bin, args) async => ProcessResult(0, 0, '{"at_login":true}', ''),
    );
    final prefs = AppPrefs(File('/nonexistent/x/desktop.json'));
    late BuildContext ctx;
    await tester.pumpWidget(
      _app((c) {
        ctx = c;
        return const SizedBox();
      }),
    );
    await tester.runAsync(() => maybeAskKeepRunning(ctx, control, prefs));
    await tester.pumpAndSettle();
    expect(find.text('Keep tern running in the background?'), findsNothing);
    expect(prefs.askedBackground, isTrue);
  });

  test('asked_background survives a save', () async {
    final dir = Directory.systemTemp.createTempSync('tern_prefs');
    addTearDown(() => dir.deleteSync(recursive: true));
    final file = File('${dir.path}/desktop.json');
    final prefs = AppPrefs.fromFile(file)..askedBackground = true;
    await prefs.save();
    final back = AppPrefs.fromFile(file);
    expect(back.askedBackground, isTrue);
    expect(back.onClose, OnClose.keepRunning);
  });
}
