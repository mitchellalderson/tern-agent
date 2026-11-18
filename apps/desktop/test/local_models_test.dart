// The local models dialog (docs/local-models.md): installed list with
// warnings, downloads with progress, add by name, remove.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/local_models.dart';

const _gb = 1 << 30;

class _Fake extends TernClient {
  _Fake() : super(const Endpoint.unix('/nonexistent'));

  var running = true;
  var installed = <LocalModel>[];
  var pulls = <LocalPull>[];
  final started = <String>[];
  final cancelled = <String>[];
  final deleted = <String>[];

  @override
  Future<LocalStatus> localStatus() async => LocalStatus(
    installed: true,
    running: running,
    ramBytes: 16 * _gb,
    diskFree: 41 * _gb,
  );

  @override
  Future<List<LocalModel>> localModels() async => installed;

  @override
  Future<List<LocalPull>> localPulls() async => pulls;

  @override
  Future<LocalPull> localPull(String model) async {
    started.add(model);
    return LocalPull(id: 'p1', model: model, phase: 'starting');
  }

  @override
  Future<void> localCancelPull(String id) async => cancelled.add(id);

  @override
  Future<void> localDelete(String model) async => deleted.add(model);
}

TernStore _store(_Fake c) => TernStore(c)
  ..link = Link.online
  ..health = const Health(pid: 1, version: '0.1.0', features: {'local_models'});

Future<void> _show(WidgetTester tester, _Fake client) async {
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(body: LocalModelsDialog(store: _store(client))),
    ),
  );
  await tester.pump();
  await tester.pump();
  addTearDown(() => tester.pumpWidget(const SizedBox()));
}

void main() {
  test('sizes read short', () {
    expect(sizeText(4_700_000_000), '4.4 GB');
    expect(sizeText(20 * _gb), '20 GB');
    expect(sizeText(820 << 20), '820 MB');
  });

  test('a model that cannot use tools, or will not fit, is flagged', () {
    const noTools = LocalModel(name: 'a', size: _gb, tools: false);
    expect(modelNote(noTools), contains("can't run tasks"));
    const big = LocalModel(name: 'b', size: 14 * _gb, tools: true);
    expect(modelNote(big, ramBytes: 16 * _gb), contains('16 GB'));
    expect(modelNote(big, ramBytes: 64 * _gb), isNull);
    expect(modelNote(const LocalModel(name: 'c', size: _gb)), isNull);
  });

  testWidgets('shows the Mac, installed models and their notes', (
    tester,
  ) async {
    final client = _Fake()
      ..installed = const [
        LocalModel(
          name: 'qwen3:8b',
          size: 5 * _gb,
          parameterSize: '8B',
          tools: true,
          loaded: true,
        ),
        LocalModel(name: 'llama2', size: 4 * _gb, tools: false),
      ];
    await _show(tester, client);
    expect(find.textContaining('16 GB Mac · 41 GB free'), findsOneWidget);
    expect(find.text('qwen3:8b'), findsOneWidget);
    expect(find.textContaining('in memory'), findsOneWidget);
    expect(find.textContaining("can't run tasks"), findsOneWidget);
  });

  testWidgets('downloads by name and shows progress with cancel', (
    tester,
  ) async {
    final client = _Fake();
    await _show(tester, client);
    await tester.enterText(
      find.byKey(const ValueKey('local-name')),
      'qwen3:8b',
    );
    await tester.tap(find.byKey(const ValueKey('local-download')));
    await tester.pump();
    expect(client.started, ['qwen3:8b']);

    client.pulls = const [
      LocalPull(
        id: 'p1',
        model: 'qwen3:8b',
        phase: 'downloading',
        completed: 2 * _gb,
        total: 4 * _gb,
      ),
    ];
    await tester.pump(const Duration(milliseconds: 1600));
    await tester.pump();
    expect(find.textContaining('50% · 2.0 GB of 4.0 GB'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('cancel-p1')));
    await tester.pump();
    expect(client.cancelled, ['p1']);
  });

  testWidgets('a refused download says why', (tester) async {
    final client = _FailingFake();
    await _show(tester, client);
    await tester.enterText(find.byKey(const ValueKey('local-name')), 'big');
    await tester.tap(find.byKey(const ValueKey('local-download')));
    await tester.pump();
    await tester.pump();
    expect(find.byKey(const ValueKey('local-error')), findsOneWidget);
    expect(find.textContaining('Only 5.0 GB free'), findsOneWidget);
  });

  testWidgets('not running: offers to start Ollama', (tester) async {
    final client = _Fake()..running = false;
    await _show(tester, client);
    expect(find.byKey(const ValueKey('local-start')), findsOneWidget);
    expect(find.byKey(const ValueKey('local-name')), findsNothing);
  });
}

class _FailingFake extends _Fake {
  @override
  Future<LocalPull> localPull(String model) async =>
      throw const TernException('Only 5.0 GB free. tern keeps 10.0 GB free.');
}
