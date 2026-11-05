// Library → Bundles and the install flow against a fake client. The
// preview is real API output (tern_client's fixture): an update that adds
// a lookalike host.
import 'dart:convert';
import 'dart:io';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/bundles.dart';

Json _fixture(String name) => jsonDecode(
  File('packages/tern_client/test/fixtures/$name').readAsStringSync(),
) as Json;

class FakeBundlesClient extends TernClient {
  FakeBundlesClient() : super(const Endpoint.unix('/nonexistent'));

  final calls = <String>[];
  var previewFixture = 'bundle_preview.json';

  @override
  Future<List<InstalledBundle>> bundles() async => [
    for (final b in _fixture('bundle_list.json')['bundles'] as List)
      InstalledBundle.fromJson(b as Json),
  ];

  @override
  Future<BundlePreview> previewBundle(String source, {String? sha256}) async {
    calls.add('preview $source');
    return BundlePreview.fromJson(_fixture(previewFixture));
  }

  @override
  Future<BundleOutcome> installBundle(
    String source, {
    required String sha256,
    String? agent,
    bool acceptNewKey = false,
  }) async {
    calls.add('install $source ${sha256.substring(0, 8)}');
    return BundleOutcome(name: 'notion-recipe');
  }

  @override
  Future<List<Schedule>> schedules() async => [];

  @override
  Future<BundleOutcome> removeBundle(String name) async {
    calls.add('remove $name');
    return BundleOutcome(name: name);
  }
}

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: child),
);

void main() {
  testWidgets('lists bundles and installs only after showing the plan', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1200, 1600);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final client = FakeBundlesClient();
    final store = TernStore(client)
      ..health = const Health(pid: 1, version: '0.1.0', features: {'bundles'});
    expect(store.canManageBundles, isTrue);
    await store.loadBundles();
    await tester.pumpWidget(
      _app(BundlesView(store: store, panelOpen: false, onTogglePanel: () {})),
    );
    await tester.pumpAndSettle();
    expect(find.text('notion-recipe 1.0.0'), findsOneWidget);
    expect(find.textContaining('signed by '), findsOneWidget);
    expect(find.text('Connectors: gmail, notion'), findsOneWidget);

    await tester.tap(find.text('Install a bundle…'));
    // The title bar also listens for double taps (window zoom).
    await tester.pump(kDoubleTapTimeout);
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byType(TextField).first,
      'https://example.com/notion.tar.zst',
    );
    await tester.tap(find.text('Review'));
    await tester.pumpAndSettle();
    expect(client.calls, ['preview https://example.com/notion.tar.zst']);

    // The plan: what changed, the lookalike, the hosts and effects, what it
    // needs. Nothing installed yet.
    expect(find.text('Update notion-recipe 1.1.0?'), findsOneWidget);
    expect(
      find.text('+ connector notion: reaches notion-api.co'),
      findsOneWidget,
    );
    expect(find.textContaining('lookalike host: notion-api.co'), findsWidgets);
    expect(
      find.textContaining('Reaches api.notion.com/v1/*, notion-api.co'),
      findsOneWidget,
    );
    expect(
      find.textContaining('create_page (write), search (read)'),
      findsOneWidget,
    );
    expect(
      find.textContaining('Secret NOTION_TOKEN for notion (not set)'),
      findsOneWidget,
    );
    expect(find.textContaining('A google account for gmail'), findsOneWidget);
    expect(find.text('Routines · installed paused'), findsOneWidget);

    await tester.tap(find.widgetWithText(FilledButton, 'Update'));
    await tester.pumpAndSettle();
    expect(client.calls.last, startsWith('install https://example.com/'));
    expect(find.text('notion-recipe updated'), findsOneWidget);
    expect(find.textContaining('Set the secret NOTION_TOKEN'), findsOneWidget);
    await tester.tap(find.text('Done'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('Remove'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Remove'));
    await tester.pumpAndSettle();
    expect(client.calls.last, 'remove notion-recipe');
  });

  testWidgets('the plan lists each agent with what it can reach', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1200, 1600);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final client = FakeBundlesClient()
      ..previewFixture = 'bundle_preview_agents.json';
    final store = TernStore(client)
      ..health = const Health(pid: 1, version: '0.1.0', features: {'bundles'});
    await tester.pumpWidget(
      _app(
        Builder(
          builder: (context) => TextButton(
            onPressed: () => showInstallBundle(
              context,
              store,
              source: 'https://example.com/pack.tar.zst',
            ),
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    expect(client.calls, ['preview https://example.com/pack.tar.zst']);

    expect(
      find.text('Agents · made at install, each with its own chat'),
      findsOneWidget,
    );
    final scout = find.byKey(const Key('bundle-agent-scout'));
    final reader = find.byKey(const Key('bundle-agent-reader'));
    expect(scout, findsOneWidget);
    expect(reader, findsOneWidget);
    Finder inCard(Finder card, String text) => find.descendant(
      of: card,
      matching: find.textContaining(text, findRichText: true),
    );
    // Scout: every built-in tool incl. shell, every connector, shared memory,
    // a ceiling: each warning stands under it, once, in the card.
    expect(inCard(scout, 'Scout'), findsWidgets);
    expect(inCard(scout, 'Memory shared'), findsOneWidget);
    expect(inCard(scout, 'builtins: all'), findsOneWidget);
    expect(
      inCard(scout, 'Autonomy ceiling: autonomous (a cap, not a grant)'),
      findsOneWidget,
    );
    expect(
      inCard(scout, 'can use every built-in tool, including shell'),
      findsOneWidget,
    );
    expect(inCard(scout, 'every connector you have'), findsOneWidget);
    expect(inCard(scout, 'reads and writes your shared memory'), findsOneWidget);
    expect(inCard(scout, 'Added capability: agent @scout: connectors: all'),
        findsOneWidget);
    expect(
      inCard(scout, 'You already have an agent @scout'),
      findsOneWidget,
    );
    // Not repeated in the general WARNINGS list.
    expect(find.text('Warnings'), findsNothing);
    expect(
      find.textContaining('can use every built-in tool'),
      findsOneWidget,
    );
    // Reader is narrow: nothing to warn about, and kept as you have it.
    expect(inCard(reader, 'No autonomy ceiling'), findsOneWidget);
    expect(inCard(reader, 'builtins: read_only'), findsOneWidget);
    expect(inCard(reader, 'leaves it as you have it'), findsOneWidget);
    expect(inCard(reader, 'every built-in'), findsNothing);
    // The conflict stops the install.
    expect(
      find.textContaining('you already have an agent @scout; rename'),
      findsOneWidget,
    );
    expect(
      tester
          .widget<FilledButton>(find.widgetWithText(FilledButton, 'Update'))
          .onPressed,
      isNull,
    );
  });

  testWidgets('a bundle that made agents says they are archived on remove', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1200, 1600);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final client = _AgentsBundlesClient();
    final store = TernStore(client)
      ..health = const Health(pid: 1, version: '0.1.0', features: {'bundles'});
    await store.loadBundles();
    await tester.pumpWidget(
      _app(BundlesView(store: store, panelOpen: false, onTogglePanel: () {})),
    );
    await tester.pumpAndSettle();
    expect(find.text('Agents: @scout, @reader'), findsOneWidget);
    await tester.tap(find.text('Remove'));
    await tester.pumpAndSettle();
    expect(
      find.textContaining('are archived, not deleted'),
      findsOneWidget,
    );
  });
}

class _AgentsBundlesClient extends FakeBundlesClient {
  @override
  Future<List<InstalledBundle>> bundles() async => [
    InstalledBundle.fromJson({
      'name': 'research-pack',
      'version': '1.0.0',
      'source': '/tmp/p.tar.zst',
      'sha256': 'aa',
      'signature': 'unsigned',
      'installed_at': 'now',
      'agents': [
        {'slug': 'scout', 'id': 'tern:scout'},
        {'slug': 'reader', 'id': 'tern:reader'},
      ],
    }),
  ];
}
