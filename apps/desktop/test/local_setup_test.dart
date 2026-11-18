// First run with Ollama: one prompt, and only what the person must do
// (docs/local-models.md).
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/local_setup.dart';

class _Fake extends TernClient {
  _Fake(this.local) : super(const Endpoint.unix('/nonexistent'));

  LocalStatus local;
  var installed = <LocalModel>[];
  var activePulls = <LocalPull>[];
  final installs = <bool>[];
  final pulls = <String>[];
  final saved = <({ProviderDraft draft, String? defaultModel})>[];
  var starts = 0;
  String? startFails;

  @override
  Future<LocalStatus> localStatus() async => local;

  @override
  Future<List<LocalModel>> localModels() async => installed;

  @override
  Future<List<LocalPull>> localPulls() async => activePulls;

  @override
  Future<LocalStatus> localInstall({required bool consent}) async {
    installs.add(consent);
    return local;
  }

  @override
  Future<LocalStatus> localStart() async {
    starts++;
    if (startFails case final f?) throw TernException(f);
    return local;
  }

  @override
  Future<LocalPull> localPull(String model) async {
    pulls.add(model);
    return LocalPull(id: 'p', model: model, phase: 'starting');
  }

  @override
  Future<ProviderStatus> saveProvider(
    ProviderDraft draft, {
    String? existing,
    String? defaultModel,
  }) async {
    saved.add((draft: draft, defaultModel: defaultModel));
    return const ProviderStatus(
      name: 'ollama',
      kind: ProviderKind.openaiCompat,
      keySource: 'none',
      isDefault: true,
    );
  }
}

const _plan = LocalInstallPlan(
  summary: 'Downloads Ollama from its official GitHub release.',
  available: true,
);
const _starter = LocalStarter(
  model: 'qwen3.5:9b',
  sizeGb: 6.6,
  blurb: 'Good all-round.',
);

Future<({int done, int api})> _show(WidgetTester tester, _Fake client) async {
  final calls = [0, 0];
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: OllamaFirstRun(
          client: client,
          onDone: () => calls[0]++,
          onUseApi: () => calls[1]++,
        ),
      ),
    ),
  );
  await tester.pump();
  await tester.pump();
  addTearDown(() => tester.pumpWidget(const SizedBox()));
  return (done: 0, api: 0);
}

void main() {
  testWidgets('not installed: says what it does, one button installs', (
    tester,
  ) async {
    final c = _Fake(const LocalStatus(install: _plan, starter: _starter));
    await _show(tester, c);
    expect(find.textContaining('official GitHub release'), findsOneWidget);
    expect(find.textContaining('qwen3.5:9b (6.6 GB)'), findsOneWidget);
    expect(c.installs, isEmpty);
    await tester.tap(find.byKey(const ValueKey('local-setup')));
    await tester.pump();
    expect(c.installs, [true]);
  });

  testWidgets('installed but off: starts by itself, no button', (tester) async {
    final c = _Fake(const LocalStatus(installed: true, starter: _starter));
    await _show(tester, c);
    expect(c.starts, 1);
    expect(find.text('Starting Ollama…'), findsOneWidget);
    expect(find.byKey(const ValueKey('local-setup')), findsNothing);
  });

  testWidgets('a failed start shows one line and a retry', (tester) async {
    final c = _Fake(const LocalStatus(installed: true, starter: _starter))
      ..startFails = 'Another program is using port 11434.';
    await _show(tester, c);
    await tester.pump();
    expect(find.textContaining('port 11434'), findsOneWidget);
    expect(find.byKey(const ValueKey('local-retry')), findsOneWidget);
    c.startFails = null;
    await tester.tap(find.byKey(const ValueKey('local-retry')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(c.starts, 2);
  });

  testWidgets('running with no model: agree once, then it downloads', (
    tester,
  ) async {
    final c = _Fake(
      const LocalStatus(installed: true, running: true, starter: _starter),
    );
    await _show(tester, c);
    expect(c.pulls, isEmpty);
    await tester.tap(find.byKey(const ValueKey('local-setup')));
    await tester.pump();
    await tester.pump();
    expect(c.pulls, ['qwen3.5:9b']);
  });

  testWidgets('a model that can use tools becomes the default', (tester) async {
    final c = _Fake(
      const LocalStatus(installed: true, running: true, starter: _starter),
    )..installed = const [LocalModel(name: 'qwen3.5:9b', tools: true)];
    await _show(tester, c);
    await tester.pump();
    expect(c.saved.single.draft.preset, 'ollama');
    expect(c.saved.single.defaultModel, 'ollama:qwen3.5:9b');
  });

  testWidgets('a model without tools is not made the default', (tester) async {
    final c = _Fake(
      const LocalStatus(installed: true, running: true, starter: _starter),
    )..installed = const [LocalModel(name: 'llama2', tools: false)];
    await _show(tester, c);
    await tester.pump();
    expect(c.saved, isEmpty);
  });

  testWidgets('download progress shows', (tester) async {
    final c =
        _Fake(
            const LocalStatus(
              installed: true,
              running: true,
              starter: _starter,
            ),
          )
          ..activePulls = const [
            LocalPull(
              id: 'p',
              model: 'qwen3.5:9b',
              phase: 'downloading',
              completed: 1,
              total: 4,
            ),
          ];
    await _show(tester, c);
    expect(find.textContaining('qwen3.5:9b · 25%'), findsOneWidget);
  });

  testWidgets('the API link is always there, with the honest line', (
    tester,
  ) async {
    final c = _Fake(const LocalStatus(install: _plan, starter: _starter));
    await _show(tester, c);
    expect(find.text('Use an API model instead'), findsOneWidget);
    expect(find.textContaining('stronger on long multi-step'), findsOneWidget);
  });
}
