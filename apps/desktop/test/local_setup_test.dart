// "Run on this Mac": the four states and the consent step
// (docs/local-models.md).
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/local_setup.dart';

class _Fake extends TernClient {
  _Fake(this.status) : super(const Endpoint.unix('/nonexistent'));

  LocalStatus status;
  final installs = <bool>[];
  var starts = 0;

  @override
  Future<LocalStatus> localStatus() async => status;

  @override
  Future<LocalStatus> localInstall({required bool consent}) async {
    installs.add(consent);
    return status;
  }

  @override
  Future<LocalStatus> localStart() async {
    starts++;
    return status;
  }
}

const _plan = LocalInstallPlan(
  source: 'github.com/ollama/ollama',
  destination: '/Users/me/Applications/Ollama.app',
  summary: 'Downloads Ollama from its official GitHub release.',
  available: true,
);

Future<void> _show(
  WidgetTester tester,
  _Fake client, {
  VoidCallback? onReady,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: LocalSetup(client: client, onReady: onReady ?? () {}),
      ),
    ),
  );
  await tester.pump();
  addTearDown(() => tester.pumpWidget(const SizedBox()));
}

void main() {
  testWidgets('not installed: says what the install does, asks first', (
    tester,
  ) async {
    final client = _Fake(const LocalStatus(install: _plan));
    await _show(tester, client);
    expect(find.text('Run on this Mac'), findsOneWidget);
    expect(find.text(_plan.summary), findsOneWidget);
    expect(client.installs, isEmpty);
    await tester.tap(find.byKey(const ValueKey('local-install')));
    await tester.pump();
    expect(client.installs, [true]);
    expect(find.byKey(const ValueKey('local-get')), findsOneWidget);
  });

  testWidgets('installing shows progress, not the buttons', (tester) async {
    final client = _Fake(
      const LocalStatus(
        install: _plan,
        installing: LocalInstalling(
          state: 'working',
          phase: 'downloading',
          completed: 50,
          total: 100,
        ),
      ),
    );
    await _show(tester, client);
    expect(find.text('Downloading Ollama…'), findsOneWidget);
    expect(find.byKey(const ValueKey('local-install')), findsNothing);
  });

  testWidgets('a failed install says why', (tester) async {
    final client = _Fake(
      const LocalStatus(
        install: _plan,
        installing: LocalInstalling(state: 'failed', message: 'No checksum.'),
      ),
    );
    await _show(tester, client);
    expect(find.text('No checksum.'), findsOneWidget);
    expect(find.byKey(const ValueKey('local-install')), findsOneWidget);
  });

  testWidgets('installed but not running: Start Ollama', (tester) async {
    final client = _Fake(const LocalStatus(installed: true, install: _plan));
    await _show(tester, client);
    await tester.tap(find.byKey(const ValueKey('local-start')));
    await tester.pump();
    expect(client.starts, 1);
  });

  testWidgets('running: warnings show and Continue hands over', (tester) async {
    var ready = 0;
    final client = _Fake(
      const LocalStatus(
        installed: true,
        running: true,
        version: '0.9.0',
        warnings: ['Ollama gives models 4,096 tokens of context.'],
      ),
    );
    await _show(tester, client, onReady: () => ready++);
    expect(find.text('Ollama 0.9.0 is running'), findsOneWidget);
    expect(find.textContaining('4,096'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('local-continue')));
    expect(ready, 1);
  });

  testWidgets('a too-old Ollama blocks Continue', (tester) async {
    var ready = 0;
    final client = _Fake(
      const LocalStatus(
        installed: true,
        running: true,
        tooOld: true,
        warnings: ['This Ollama (0.5.0) is too old.'],
      ),
    );
    await _show(tester, client, onReady: () => ready++);
    await tester.tap(
      find.byKey(const ValueKey('local-continue')),
      warnIfMissed: false,
    );
    expect(ready, 0);
    expect(find.textContaining('too old'), findsOneWidget);
  });
}
