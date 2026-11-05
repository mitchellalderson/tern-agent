// Library → MCP servers against a fake client
// (docs/mcp-management-spec.md §8.2): the list, details, and the add
// sheet's states.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/mcp.dart';
import 'package:tern_desktop/ui/mcp_sheet.dart';
import 'package:tern_desktop/ui/sidebar.dart';
import 'package:tern_desktop/ui/tools.dart';

const _tools = [
  McpToolInfo(
    exposed: 'mcp__github__search',
    remote: 'search',
    description: 'Search code',
  ),
  McpToolInfo(
    exposed: 'mcp__github__delete_repo',
    remote: 'delete_repo',
    description: 'Delete a repository',
  ),
];

McpStatus _github({bool enabled = true, List<String> disabled = const []}) =>
    McpStatus(
      name: 'github',
      config: McpServerConfig(
        command: 'github-mcp-server',
        args: const ['stdio'],
        envSecrets: const ['GITHUB_TOKEN'],
        enabled: enabled,
        disabledTools: disabled,
      ),
      state: enabled
          ? McpRunning(tools: 2 - disabled.length, since: DateTime(2026))
          : const McpDisabled(),
      tools: [
        for (final t in _tools)
          McpToolInfo(
            exposed: t.exposed,
            remote: t.remote,
            description: t.description,
            disabled: disabled.contains(t.remote),
          ),
      ],
      serverInfo: 'github-mcp-server 0.4.1',
      stderrTail: const ['listening on stdio'],
    );

final _postgres = McpStatus(
  name: 'postgres',
  config: const McpServerConfig(command: 'pg-mcp'),
  state: McpUnavailable(
    error: '`pg-mcp` exited (FATAL: password authentication failed)',
    retryAt: DateTime.now().add(const Duration(seconds: 30)),
    failures: 2,
  ),
);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  var servers = <McpStatus>[_github(), _postgres];
  final patches = <String>[];
  final verifies = <({McpServerConfig config, Map<String, String> secrets})>[];
  final adds =
      <({String name, McpServerConfig config, Map<String, String> secrets})>[];
  final removed = <String>[];
  var verifyResult = McpVerifyResult(
    verified: const McpVerified(
      tools: _tools,
      serverInfo: 'github-mcp-server 0.4.1',
    ),
  );
  var known = <SecretStatus>[];

  @override
  Future<List<McpStatus>> mcpServers() async => servers;

  @override
  Future<McpStatus> mcpServer(String name) async =>
      servers.firstWhere((s) => s.name == name);

  @override
  Future<List<SecretStatus>> secrets() async => known;

  @override
  Future<McpStatus> patchMcp(
    String name, {
    bool? enabled,
    List<String>? disabledTools,
    bool? requiresApproval,
    bool? idempotent,
  }) async {
    patches.add('$name enabled=$enabled disabled=$disabledTools');
    if (name == 'github') {
      servers = [
        _github(enabled: enabled ?? true, disabled: disabledTools ?? const []),
        _postgres,
      ];
    }
    return servers.first;
  }

  @override
  Future<McpVerifyResult> verifyMcp(
    String name,
    McpServerConfig config, {
    Map<String, String> secrets = const {},
  }) async {
    verifies.add((config: config, secrets: secrets));
    return verifyResult;
  }

  @override
  Future<McpStatus> addMcp(
    String name,
    McpServerConfig config, {
    Map<String, String> secrets = const {},
  }) async {
    adds.add((name: name, config: config, secrets: secrets));
    final s = McpStatus(
      name: name,
      config: config,
      state: const McpConnecting(),
    );
    servers = [...servers, s];
    return s;
  }

  @override
  Future<void> removeMcp(String name, {bool removeSecrets = false}) async {
    removed.add('$name $removeSecrets');
    servers = servers.where((s) => s.name != name).toList();
  }
}

TernStore _store(_FakeClient client, {String auth = 'owner'}) =>
    TernStore(client)
      ..link = Link.online
      ..health = Health(
        pid: 1,
        version: '0.1.0',
        authKind: auth,
        features: const {'providers', 'mcp'},
      )
      ..info = const Info(
        tools: [
          (name: 'shell', description: 'Run a command'),
          (name: 'mcp__github__search', description: 'Search code'),
        ],
      )
      ..mcpServers = client.servers;

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: child),
);

const _draft = ImportedServer(
  name: 'github',
  config: McpServerConfig(
    command: 'npx',
    args: ['-y', '@modelcontextprotocol/server-github'],
    envSecrets: ['GITHUB_TOKEN'],
  ),
  secrets: {'GITHUB_TOKEN': null},
  warnings: ['GITHUB_TOKEN was a placeholder; enter the real value.'],
);

Future<void> _pumpSheet(
  WidgetTester tester,
  TernStore store, {
  List<ImportedServer> drafts = const [],
}) async {
  tester.view.physicalSize = const Size(1000, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  // A fresh sheet each time, not the last one's state.
  await tester.pumpWidget(const SizedBox());
  await tester.pumpWidget(_app(McpSheet(store: store, drafts: drafts)));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('the list shows state, errors and a toggle', (tester) async {
    final client = _FakeClient();
    final store = _store(client);
    await tester.pumpWidget(_app(McpList(store: store)));
    expect(find.text('github'), findsOneWidget);
    expect(find.text('running · 2 tools'), findsOneWidget);
    expect(find.text('stdio'), findsNWidgets(2));
    expect(find.text('unavailable · 2 failures'), findsOneWidget);
    expect(find.textContaining('password authentication'), findsOneWidget);
    expect(find.textContaining(RegExp(r'retry in \d+s')), findsOneWidget);

    await tester.tap(find.byType(Switch).first);
    await tester.pump();
    expect(client.patches.single, 'github enabled=false disabled=null');
    expect(store.mcpServers.first.state, isA<McpDisabled>());

    await tester.tap(find.text('github'));
    expect(store.page, LibraryPage.mcp);
    expect(store.mcpSelected, 'github');
  });

  testWidgets('details toggle tools, show logs and confirm removal', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1000, 1000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final client = _FakeClient();
    final store = _store(client)..openMcp('github');
    await tester.pumpWidget(
      _app(McpDetail(store: store, server: store.mcpServers.first)),
    );
    await tester.pumpAndSettle();
    expect(find.textContaining('github-mcp-server 0.4.1'), findsOneWidget);
    expect(find.text('github-mcp-server stdio'), findsOneWidget);

    // Tool switches come after nothing else on this page.
    await tester.tap(find.byType(Switch).at(1));
    await tester.pumpAndSettle();
    expect(client.patches.single, 'github enabled=null disabled=[delete_repo]');

    expect(find.text('listening on stdio'), findsNothing);
    await tester.tap(find.textContaining('Logs'));
    await tester.pumpAndSettle();
    expect(find.text('listening on stdio'), findsOneWidget);

    await tester.tap(find.text('Remove'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Also delete its secrets'), findsOneWidget);
    await tester.tap(find.widgetWithText(FilledButton, 'Remove'));
    await tester.pumpAndSettle();
    expect(client.removed.single, 'github true');
    expect(store.mcpSelected, isNull);
  });

  testWidgets('a missing secret is asked for before testing', (tester) async {
    final client = _FakeClient();
    final store = _store(client);
    await _pumpSheet(tester, store, drafts: const [_draft]);
    expect(find.textContaining('placeholder'), findsOneWidget);
    expect(find.text('GITHUB_TOKEN · not set'), findsOneWidget);
    expect(find.text('-y'), findsOneWidget);

    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Enter GITHUB_TOKEN first'), findsOneWidget);
    expect(client.verifies, isEmpty);

    await tester.enterText(
      find.byKey(const ValueKey('mcp-secret-GITHUB_TOKEN')),
      'ghp_live',
    );
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    expect(client.verifies.single.secrets, {'GITHUB_TOKEN': 'ghp_live'});
    expect(
      find.text('Connected — github-mcp-server 0.4.1, 2 tools'),
      findsOneWidget,
    );

    // Unchecked tools are saved as disabled.
    await tester.tap(find.text('delete_repo'));
    await tester.pump();
    expect(find.text('Tools · 1 of 2 on'), findsOneWidget);
    await tester.tap(find.widgetWithText(FilledButton, 'Save'));
    await tester.pumpAndSettle();
    final add = client.adds.single;
    expect(add.name, 'github');
    expect(add.config.disabledTools, ['delete_repo']);
    expect(add.secrets, {'GITHUB_TOKEN': 'ghp_live'});
    expect(store.mcpServers.map((s) => s.name), contains('github'));
  });

  testWidgets('a secret already set is not asked for', (tester) async {
    final client = _FakeClient()
      ..known = [
        const SecretStatus(
          name: 'GITHUB_TOKEN',
          inFile: false,
          inEnv: true,
          usedBy: [],
        ),
      ];
    await _pumpSheet(tester, _store(client), drafts: const [_draft]);
    expect(find.text('set (env)'), findsOneWidget);
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    expect(client.verifies.single.secrets, isEmpty);
  });

  testWidgets('a failed test shows the error and stderr', (tester) async {
    final client = _FakeClient()
      ..verifyResult = const McpVerifyResult(
        error: McpVerifyError(
          kind: 'exited_early',
          message: 'failed to start: connection closed',
          stderrTail: ['Error: GITHUB_TOKEN is invalid'],
        ),
      );
    final store = _store(client);
    await _pumpSheet(tester, store, drafts: const [_draft]);
    await tester.enterText(
      find.byKey(const ValueKey('mcp-secret-GITHUB_TOKEN')),
      'bad',
    );
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    expect(find.text('Exited during startup'), findsOneWidget);
    expect(find.text('failed to start: connection closed'), findsOneWidget);
    expect(find.text('Error: GITHUB_TOKEN is invalid'), findsOneWidget);

    // It can still be saved, e.g. before its dependency is installed.
    await tester.tap(find.text('Save without testing'));
    await tester.pumpAndSettle();
    expect(client.adds.single.name, 'github');
  });

  testWidgets('a remote core can only add URLs', (tester) async {
    final client = _FakeClient();
    final store = _store(client, auth: 'device');
    await _pumpSheet(tester, store);
    expect(find.text(remoteStdioOff), findsOneWidget);
    await tester.tap(find.text('Local command'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('mcp-command')), findsNothing);

    await tester.tap(find.text('Remote URL'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('mcp-url')), findsOneWidget);

    // A pasted stdio server can't be tested or saved from here.
    await _pumpSheet(tester, store, drafts: const [_draft]);
    expect(find.text(remoteStdioOff), findsOneWidget);
    final test = tester.widget<FilledButton>(
      find.widgetWithText(FilledButton, 'Test connection'),
    );
    expect(test.onPressed, isNull);
  });

  testWidgets('make secret moves an env row to env_secrets', (tester) async {
    final client = _FakeClient();
    await _pumpSheet(
      tester,
      _store(client),
      drafts: const [
        ImportedServer(
          name: 'linear',
          config: McpServerConfig(
            command: 'linear-mcp',
            env: {'LINEAR_API_KEY': 'lin_api_x', 'LOG_LEVEL': 'warn'},
          ),
        ),
      ],
    );
    await tester.tap(find.text('Make secret').first);
    await tester.pumpAndSettle();
    expect(find.text('LINEAR_API_KEY · not set'), findsOneWidget);
    await tester.tap(find.text('Save without testing'));
    await tester.pumpAndSettle();
    final add = client.adds.single;
    expect(add.config.env, {'LOG_LEVEL': 'warn'});
    expect(add.config.envSecrets, ['LINEAR_API_KEY']);
    expect(add.secrets, {'LINEAR_API_KEY': 'lin_api_x'});
  });

  testWidgets('several pasted servers are tabs', (tester) async {
    final client = _FakeClient();
    await _pumpSheet(
      tester,
      _store(client),
      drafts: const [
        ImportedServer(
          name: 'docs',
          config: McpServerConfig(
            transport: McpTransport.http,
            url: 'https://docs.example.com/mcp',
          ),
        ),
        ImportedServer(
          name: 'files',
          config: McpServerConfig(command: 'files-mcp'),
        ),
      ],
    );
    expect(find.text('Review 2 pasted servers'), findsOneWidget);
    await tester.tap(find.text('Save without testing').first);
    await tester.pumpAndSettle();
    expect(client.adds.single.name, 'docs');
    // On to the second tab.
    expect(find.byType(McpSheet), findsOneWidget);
    await tester.tap(find.text('Save without testing').last);
    await tester.pumpAndSettle();
    expect(client.adds.map((a) => a.name), ['docs', 'files']);
    expect(find.byType(McpSheet), findsNothing);
  });

  testWidgets('the library entry needs the mcp feature', (tester) async {
    final client = _FakeClient();
    final store = _store(client);
    await tester.pumpWidget(_app(Sidebar(store: store)));
    expect(find.text('MCP servers'), findsOneWidget);
    store.health = const Health(pid: 1, version: '0.1.0');
    await tester.pumpWidget(_app(Sidebar(store: store)));
    expect(find.text('MCP servers'), findsNothing);
  });

  testWidgets('side panel groups mcp tools under their server', (tester) async {
    final client = _FakeClient();
    final store = _store(client);
    await tester.pumpWidget(
      _app(SingleChildScrollView(child: ToolsSection(store: store))),
    );
    // The list starts folded; open it.
    expect(find.text('shell'), findsNothing);
    await tester.tap(find.textContaining('Tools ·'));
    await tester.pump();
    expect(find.text('shell'), findsOneWidget);
    expect(find.text('github'), findsOneWidget);
    expect(find.text('search'), findsOneWidget);
    // Down, so no tools, but still listed with its state.
    expect(find.text('postgres'), findsOneWidget);
    expect(find.text('unavailable · 2 failures'), findsOneWidget);
    await tester.tap(find.text('postgres'));
    expect(store.mcpSelected, 'postgres');
  });

  test('mcp_changed refreshes the list', () async {
    final client = _FakeClient();
    final store = _store(client)..mcpServers = [];
    await store.loadMcp();
    expect(store.mcpServers, hasLength(2));
  });
}
