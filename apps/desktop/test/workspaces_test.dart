// The folder picker UI against an in-memory WorkspaceApi (W2 of
// docs/alpha-workspaces.md).
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/daemon.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/workspaces.dart';
import 'package:tern_desktop/state/workspaces_adapter.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/channel_view.dart';
import 'package:tern_desktop/ui/preferences.dart';
import 'package:tern_desktop/ui/workspace_ui.dart';

import 'support/fake_workspaces.dart';

class _Client extends TernClient {
  _Client() : super(const Endpoint.unix('/nonexistent'));
}

TernStore _store({FakeWorkspaces? api, Set<String> features = const {}}) {
  final s = TernStore(_Client())
    ..health = Health(pid: 1, version: '0', features: features);
  if (api != null) s.workspaces = api;
  return s;
}

Future<void> _host(
  WidgetTester tester,
  Widget child, {
  bool dark = false,
  double width = 900,
}) async {
  tester.view.physicalSize = Size(width, 1400);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final c = dark ? TernColors.dark : TernColors.light;
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(c, dark ? Brightness.dark : Brightness.light),
      home: Scaffold(body: SingleChildScrollView(child: child)),
    ),
  );
  await tester.pump();
}

Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 5; i++) {
    await tester.pump(const Duration(milliseconds: 60));
  }
}

class _Row extends StatefulWidget {
  const _Row(this.api, {this.initial});
  final FakeWorkspaces api;
  final String? initial;
  @override
  State<_Row> createState() => _RowState();
}

class _RowState extends State<_Row> {
  late String? path = widget.initial;
  @override
  Widget build(BuildContext context) => FolderRow(
    api: widget.api,
    path: path,
    onChosen: (p) async {
      setState(() => path = p);
      return null;
    },
    onClear: () => setState(() => path = null),
  );
}

void main() {
  late String? picked;
  late List<String> revealed;
  late List<String> opened;

  setUp(() {
    picked = '/Users/me/projects/site';
    revealed = [];
    opened = [];
    WorkspaceEnv.picker = (_) async => picked;
    WorkspaceEnv.reveal = (p) async => revealed.add(p);
    WorkspaceEnv.openUrl = (u) async => opened.add(u);
  });

  group('feature check', () {
    test('an older core, or an unwired adapter, hides the folder UI', () {
      expect(kWorkspacesWired, isFalse);
      expect(_store().workspaces, isNull);
      expect(_store(features: {workspacesFeature}).workspaces, isNull);
      expect(_store(api: FakeWorkspaces()).workspaces, isNotNull);
    });

    test('the adapter stub throws until it is wired', () {
      final api = TernWorkspaceApi(_store());
      expect(() => api.list(), throwsA(isA<UnimplementedError>()));
    });

    testWidgets('no Settings > Folders on a core without it', (tester) async {
      await _prefs(tester, _store());
      expect(find.byKey(const Key('settings-nav-folders')), findsNothing);
    });

    testWidgets('the new-channel sheet has no Folder row without it', (
      tester,
    ) async {
      await _host(tester, ChannelDialog(store: _store()));
      expect(find.byKey(const Key('folder-choose')), findsNothing);
    });
  });

  group('folder row and picker', () {
    testWidgets('Choose... shows the core verdict, then sets the folder', (
      tester,
    ) async {
      final api = FakeWorkspaces();
      await _host(tester, _Row(api));
      expect(find.text('Default folder'), findsWidgets);
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(find.text('Use this folder?'), findsOneWidget);
      expect(find.text('/Users/me/projects/site'), findsOneWidget);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(
        tester.widget<Text>(find.byKey(const Key('folder-value'))).data,
        '/Users/me/projects/site',
      );
      expect(find.byKey(const Key('folder-reveal')), findsOneWidget);
      await tester.tap(find.byKey(const Key('folder-reveal')));
      expect(revealed, ['/Users/me/projects/site']);
      await tester.tap(find.byKey(const Key('folder-use-default')));
      await tester.pump();
      expect(find.byKey(const Key('folder-reveal')), findsNothing);
    });

    testWidgets('cancelling the picker changes nothing', (tester) async {
      picked = null;
      await _host(tester, _Row(FakeWorkspaces()));
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(find.text('Use this folder?'), findsNothing);
    });

    for (final (path, words) in [
      ('/', 'root of the disk'),
      ('/Users/me', 'your home folder'),
      ('/System', 'system folder'),
      ('/Users/me/.tern', "Tern's own data"),
    ]) {
      testWidgets('the core refuses $path in its own words', (tester) async {
        picked = path;
        await _host(tester, _Row(FakeWorkspaces()));
        await tester.tap(find.byKey(const Key('folder-choose')));
        await _settle(tester);
        expect(find.byKey(const Key('folder-refusal')), findsOneWidget);
        expect(find.textContaining(words), findsOneWidget);
        final use = tester.widget<FilledButton>(
          find.byKey(const Key('folder-use')),
        );
        expect(use.onPressed, isNull, reason: 'no override');
        expect(find.byKey(const Key('folder-another')), findsOneWidget);
      });
    }

    testWidgets('Choose another... re-opens the picker and re-checks', (
      tester,
    ) async {
      picked = '/';
      final api = FakeWorkspaces();
      await _host(tester, _Row(api));
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      picked = '/Users/me/ok';
      await tester.tap(find.byKey(const Key('folder-another')));
      await _settle(tester);
      expect(find.byKey(const Key('folder-refusal')), findsNothing);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(
        tester.widget<Text>(find.byKey(const Key('folder-value'))).data,
        '/Users/me/ok',
      );
    });

    testWidgets('a warning asks to confirm: Use anyway', (tester) async {
      final api = FakeWorkspaces()
        ..warn['/Users/me/projects/site'] = [
          'This folder is in iCloud Drive. Files may be evicted.',
        ];
      await _host(tester, _Row(api));
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(find.byKey(const Key('folder-warning')), findsOneWidget);
      expect(find.text('Use anyway'), findsOneWidget);
    });

    testWidgets('macOS denied: the instruction and a link to the pane', (
      tester,
    ) async {
      final api = FakeWorkspaces()..deniedPaths.add('/Users/me/projects/site');
      await _host(tester, _Row(api));
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(
        find.textContaining(
          "macOS didn't let Tern open this folder. Allow it in System "
          'Settings > Privacy & Security > Files and Folders.',
        ),
        findsOneWidget,
      );
      if (Platform.isMacOS) {
        await tester.tap(find.byKey(const Key('open-privacy')));
        expect(opened, [WorkspaceEnv.privacyPane]);
      }
    });

    testWidgets('a symlink shows where it really goes', (tester) async {
      final api = _Canon();
      await _host(tester, _Row(api));
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(find.textContaining('shortcut'), findsOneWidget);
      expect(find.text('/Volumes/Real/site'), findsOneWidget);
    });

    testWidgets('a core that fails to answer is said plainly', (tester) async {
      await _host(tester, _Row(_Broken()));
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(find.byKey(const Key('folder-failure')), findsOneWidget);
    });
  });

  group('chip and shell state', () {
    Future<void> chip(
      WidgetTester tester,
      FakeWorkspaces api, {
      WorkspaceTarget target = const WorkspaceTarget.channel('c1'),
      bool home = false,
      void Function(String)? onHome,
      bool dark = false,
      double width = 900,
    }) async {
      await _host(
        tester,
        WorkspaceChip(
          api: api,
          target: target,
          homeTopic: home,
          onChangeHome: onHome,
        ),
        dark: dark,
        width: width,
      );
      await _settle(tester);
    }

    testWidgets('shows the folder name', (tester) async {
      await chip(tester, FakeWorkspaces());
      expect(find.text('tern-workspace'), findsOneWidget);
      expect(find.bySemanticsLabel(RegExp('Folder tern-workspace')), findsOne);
    });

    for (final (state, headline, detail, icon) in [
      (
        ShellBound.bounded,
        'Sandboxed',
        'Commands can change files only in tern-workspace. They can still '
            'read some system files.',
        Icons.shield_outlined,
      ),
      (
        ShellBound.unverified,
        'Container, not verified',
        'Commands run in a container that only sees tern-workspace. Tern has '
            'not verified that.',
        Icons.inventory_2_outlined,
      ),
      (
        ShellBound.unbounded,
        'Commands not confined',
        "This Mac isn't sandboxing commands. A command can read or change "
            'anything you can. Tern asks before every one.',
        Icons.warning_amber_outlined,
      ),
    ]) {
      testWidgets('shell state $state: copy and icon', (tester) async {
        await chip(tester, FakeWorkspaces(shell: state), dark: true);
        expect(
          tester.widget<Icon>(find.byKey(const Key('chip-shell-icon'))).icon,
          icon,
        );
        await tester.tap(find.byKey(const Key('workspace-chip')));
        await _settle(tester);
        expect(find.text(headline), findsOneWidget);
        expect(find.text(detail), findsOneWidget);
        expect(find.textContaining('File tools: Tern confines'), findsOne);
        expect(find.textContaining('Connectors that run programs'), findsOne);
      });
    }

    testWidgets('unbounded says commands are not confined, and why it asks', (
      tester,
    ) async {
      await chip(tester, FakeWorkspaces());
      await tester.tap(find.byKey(const Key('workspace-chip')));
      await _settle(tester);
      expect(find.textContaining('NOT confined'), findsOneWidget);
      expect(find.textContaining('Unattended runs never run'), findsOneWidget);
      expect(find.textContaining('tern doctor'), findsOneWidget);
    });

    testWidgets('detail names where the folder comes from', (tester) async {
      final api = FakeWorkspaces();
      await chip(tester, api);
      await tester.tap(find.byKey(const Key('workspace-chip')));
      await _settle(tester);
      expect(
        find.text('From your default folder'),
        findsOneWidget,
        reason: 'inherits',
      );
      expect(find.byKey(const Key('folder-use-default')), findsNothing);
      await tester.tap(find.byKey(const Key('folder-change')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(api.calls, contains('set channel:c1 /Users/me/projects/site'));
      expect(find.text('Chosen for this channel'), findsOneWidget);
      await tester.tap(find.byKey(const Key('folder-use-default')));
      await _settle(tester);
      expect(api.calls, contains('clear channel:c1'));
      expect(find.text('From your default folder'), findsOneWidget);
    });

    testWidgets('an agent default shows as such', (tester) async {
      final api = FakeWorkspaces();
      await api.set(const WorkspaceTarget.agent('a1'), '/Users/me/agent');
      await chip(tester, api, target: const WorkspaceTarget.agent('a1'));
      await tester.tap(find.byKey(const Key('workspace-chip')));
      await _settle(tester);
      expect(find.text('Chosen for this channel'), findsNothing);
      expect(find.text('From the agent default'), findsOneWidget);
    });

    testWidgets('read-only extra folders: add and remove', (tester) async {
      final api = FakeWorkspaces();
      await chip(tester, api);
      await tester.tap(find.byKey(const Key('workspace-chip')));
      await _settle(tester);
      expect(find.textContaining('None.'), findsOneWidget);
      picked = '/Users/me/docs';
      await tester.tap(find.byKey(const Key('add-root')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(find.text('/Users/me/docs'), findsOneWidget);
      await tester.tap(find.byTooltip('Remove /Users/me/docs'));
      await _settle(tester);
      expect(find.text('/Users/me/docs'), findsNothing);
      expect(api.calls.where((c) => c.startsWith('removeRoot')), hasLength(1));
    });

    testWidgets('a refused Change... keeps the old folder and says why', (
      tester,
    ) async {
      final api = FakeWorkspaces();
      // Valid in the sheet, refused when binding (e.g. it changed since).
      final stale = _Stale(api);
      await _host(
        tester,
        WorkspaceDetailDialog(
          api: stale,
          target: const WorkspaceTarget.channel('c1'),
        ),
      );
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-change')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(find.byKey(const Key('folder-error')), findsOneWidget);
      expect(find.textContaining('changed since'), findsOneWidget);
    });

    testWidgets('Home topic: Change folder starts a new topic', (tester) async {
      final api = FakeWorkspaces();
      String? started;
      await chip(
        tester,
        api,
        target: const WorkspaceTarget.channel('home'),
        home: true,
        onHome: (p) => started = p,
      );
      await tester.tap(find.byKey(const Key('workspace-chip')));
      await _settle(tester);
      expect(find.text('Change folder (starts a new topic)'), findsOneWidget);
      expect(find.byKey(const Key('folder-use-default')), findsNothing);
      await tester.tap(find.byKey(const Key('folder-change')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(started, '/Users/me/projects/site');
      expect(
        api.calls.where((c) => c.startsWith('set ')),
        isEmpty,
        reason: 'nothing is rebound under the running topic',
      );
    });

    testWidgets('narrow window, dark: no overflow', (tester) async {
      await chip(tester, FakeWorkspaces(), dark: true, width: 320);
      await tester.tap(find.byKey(const Key('workspace-chip')));
      await _settle(tester);
      final e = tester.takeException();
      expect(
        e,
        isNull,
        reason: e is FlutterError
            ? e.diagnostics.map((d) => d.toStringDeep()).join('\n')
            : '',
      );
    });

    testWidgets('keyboard: the chip is focusable and Enter opens it', (
      tester,
    ) async {
      await chip(tester, FakeWorkspaces());
      await tester.sendKeyEvent(LogicalKeyboardKey.tab);
      await tester.pump();
      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await _settle(tester);
      expect(find.text('Folder'), findsWidgets);
      expect(find.byKey(const Key('shell-headline')), findsOneWidget);
    });
  });

  group('missing, replaced, denied', () {
    for (final (status, words) in [
      (WorkspaceStatus.missing, 'will not create it again'),
      (WorkspaceStatus.replaced, 'no longer the folder you chose'),
      (WorkspaceStatus.denied, "macOS didn't let Tern open this folder"),
    ]) {
      testWidgets('banner for $status, with Locate or Choose another', (
        tester,
      ) async {
        final api = FakeWorkspaces()
          ..markStatus('/Users/me/tern-workspace', status);
        await _host(
          tester,
          WorkspaceBanner(
            api: api,
            target: const WorkspaceTarget.channel('c1'),
          ),
        );
        await _settle(tester);
        expect(find.byKey(Key('workspace-card-${status.name}')), findsOne);
        expect(find.textContaining(words), findsOneWidget);
        // Fixing it: Locate / Choose another binds a good folder.
        await tester.tap(find.byKey(const Key('card-locate')));
        await _settle(tester);
        await tester.tap(find.byKey(const Key('folder-use')));
        await _settle(tester);
        expect(find.byKey(Key('workspace-card-${status.name}')), findsNothing);
      });
    }

    testWidgets('a healthy folder shows no banner', (tester) async {
      await _host(
        tester,
        WorkspaceBanner(
          api: FakeWorkspaces(),
          target: const WorkspaceTarget.channel('c1'),
        ),
      );
      await _settle(tester);
      expect(find.byType(WorkspaceStatusCard), findsNothing);
    });

    testWidgets('chip marks a missing folder as unavailable', (tester) async {
      final api = FakeWorkspaces()
        ..markStatus('/Users/me/tern-workspace', WorkspaceStatus.missing);
      await _host(
        tester,
        WorkspaceChip(api: api, target: const WorkspaceTarget.channel('c1')),
      );
      await _settle(tester);
      expect(
        tester.widget<Icon>(find.byKey(const Key('chip-shell-icon'))).icon,
        Icons.error_outline,
      );
      expect(find.bySemanticsLabel(RegExp('unavailable')), findsOneWidget);
    });
  });

  group('remote core', () {
    testWidgets('no native picker: Browse... opens the server browser', (
      tester,
    ) async {
      var nativeCalls = 0;
      WorkspaceEnv.picker = (_) async {
        nativeCalls++;
        return null;
      };
      final api = FakeWorkspaces(isLocal: false);
      await _host(tester, _Row(api, initial: '/home/me/notes'));
      expect(find.text('Choose...'), findsNothing);
      expect(find.text('Browse...'), findsOneWidget);
      expect(
        find.byKey(const Key('folder-reveal')),
        findsNothing,
        reason: 'the folder is not on this computer',
      );
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(nativeCalls, 0);
      expect(find.text('Choose a folder on the server'), findsOneWidget);
      expect(find.textContaining('not on this one'), findsOneWidget);
    });

    testWidgets('walk down, up, and pick; refusals show', (tester) async {
      final api = FakeWorkspaces(isLocal: false)
        ..refuse['/home/me'] = 'This folder is not allowed: it is home.';
      await _host(tester, _Row(api));
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(find.byKey(const Key('folder-refusal')), findsOneWidget);
      expect(
        tester
            .widget<FilledButton>(find.byKey(const Key('folder-use')))
            .onPressed,
        isNull,
      );
      await tester.tap(find.byKey(const Key('browse-projects')));
      await _settle(tester);
      expect(find.byKey(const Key('browse-site')), findsOneWidget);
      expect(find.byKey(const Key('folder-refusal')), findsNothing);
      await tester.tap(find.byKey(const Key('browse-site')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('browse-up')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('browse-api')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(
        tester.widget<Text>(find.byKey(const Key('folder-value'))).data,
        '/home/me/projects/api',
      );
    });
  });

  group('new channel and agent sheets', () {
    testWidgets('new channel: a Folder row with the default shown', (
      tester,
    ) async {
      final api = FakeWorkspaces();
      await _host(tester, ChannelDialog(store: _store(api: api)));
      expect(find.byKey(const Key('folder-choose')), findsOneWidget);
      expect(
        tester.widget<Text>(find.byKey(const Key('folder-value'))).data,
        'Default folder',
      );
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(
        tester.widget<Text>(find.byKey(const Key('folder-value'))).data,
        '/Users/me/projects/site',
      );
    });

    testWidgets('agent default folder saves right away, and clears', (
      tester,
    ) async {
      final api = FakeWorkspaces();
      await _host(tester, AgentFolderField(api: api, agentId: 'a1'));
      await _settle(tester);
      expect(find.textContaining('Your default folder'), findsOneWidget);
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(api.calls, contains('set agent:a1 /Users/me/projects/site'));
      expect(
        tester.widget<Text>(find.byKey(const Key('folder-value'))).data,
        '/Users/me/projects/site',
      );
      await tester.tap(find.byKey(const Key('folder-use-default')));
      await _settle(tester);
      expect(api.calls, contains('clear agent:a1'));
      expect(find.textContaining('Your default folder'), findsOneWidget);
    });

    testWidgets('agent default refused: the core message is shown', (
      tester,
    ) async {
      final api = FakeWorkspaces();
      picked = '/Users/me/projects/site';
      final stale = _Stale(api);
      await _host(tester, AgentFolderField(api: stale, agentId: 'a1'));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(find.byKey(const Key('folder-error')), findsOneWidget);
    });
  });

  group('Settings > Folders', () {
    testWidgets('default folder, overrides, extra folders, Use default', (
      tester,
    ) async {
      final api = FakeWorkspaces();
      await api.set(const WorkspaceTarget.channel('fin'), '/Users/me/fin');
      await api.set(const WorkspaceTarget.agent('bot'), '/Users/me/bot');
      final store = _store(api: api);
      await _prefs(tester, store);
      expect(find.byKey(const Key('settings-nav-folders')), findsOneWidget);
      expect(find.text('/Users/me/tern-workspace'), findsWidgets);
      expect(find.byKey(const Key('override-channel-fin')), findsOneWidget);
      expect(find.byKey(const Key('override-agent-bot')), findsOneWidget);

      // Underline tabs filter the list.
      await tester.tap(find.byKey(const Key('folders-tab-agents')));
      await tester.pump();
      expect(find.byKey(const Key('override-channel-fin')), findsNothing);
      expect(find.byKey(const Key('override-agent-bot')), findsOneWidget);
      await tester.tap(find.byKey(const Key('folders-tab-topics')));
      await tester.pump();
      expect(find.text('Nothing here has its own folder.'), findsOneWidget);
      await tester.tap(find.byKey(const Key('folders-tab-all')));
      await tester.pump();

      // Use default drops the override.
      await tester.tap(
        find.descendant(
          of: find.byKey(const Key('override-channel-fin')),
          matching: find.byKey(const Key('override-use-default')),
        ),
      );
      await _settle(tester);
      expect(find.byKey(const Key('override-channel-fin')), findsNothing);

      // Read-only extra folders.
      picked = '/Users/me/ref';
      await tester.ensureVisible(find.byKey(const Key('add-root-ws0')));
      await tester.tap(find.byKey(const Key('add-root-ws0')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(find.text('/Users/me/ref'), findsOneWidget);
      await tester.tap(find.byTooltip('Remove /Users/me/ref'));
      await _settle(tester);
      expect(find.text('/Users/me/ref'), findsNothing);
    });

    testWidgets('changing the default; a refusal keeps the old one', (
      tester,
    ) async {
      final api = FakeWorkspaces();
      await _prefs(tester, _store(api: api));
      picked = '/System';
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      expect(find.textContaining('system folder'), findsOneWidget);
      await tester.tap(find.text('Cancel'));
      await _settle(tester);
      picked = '/Users/me/new-default';
      await tester.tap(find.byKey(const Key('folder-choose')));
      await _settle(tester);
      await tester.tap(find.byKey(const Key('folder-use')));
      await _settle(tester);
      expect(api.calls, contains('setDefault /Users/me/new-default'));
      expect(find.text('/Users/me/new-default'), findsWidgets);
    });

    testWidgets('a missing default shows the card', (tester) async {
      final api = FakeWorkspaces()
        ..markStatus('/Users/me/tern-workspace', WorkspaceStatus.missing);
      await _prefs(tester, _store(api: api));
      expect(find.byKey(const Key('workspace-card-missing')), findsOneWidget);
    });

    testWidgets('dark theme', (tester) async {
      await _prefs(tester, _store(api: FakeWorkspaces()), dark: true);
      expect(tester.takeException(), isNull);
    });
  });
}

Future<void> _prefs(
  WidgetTester tester,
  TernStore store, {
  bool dark = false,
}) async {
  tester.view.physicalSize = const Size(1100, 800);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final c = dark ? TernColors.dark : TernColors.light;
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(c, dark ? Brightness.dark : Brightness.light),
      home: Scaffold(
        body: PreferencesDialog(
          control: TernControl(bin: null, env: const {'HOME': '/nonexistent'}),
          prefs: AppPrefs(File('/nonexistent/desktop.json')),
          store: store,
          section: store.workspaces == null
              ? PrefSection.general
              : PrefSection.folders,
        ),
      ),
    ),
  );
  await _settle(tester);
}

/// A canonical path that differs from the chosen one.
class _Canon extends FakeWorkspaces {
  @override
  Future<Verdict> validate(String path) async =>
      const Verdict(canonical: '/Volumes/Real/site');
}

class _Broken extends FakeWorkspaces {
  @override
  Future<Verdict> validate(String path) async => throw StateError('down');
}

/// Passes validation but refuses to bind.
class _Stale extends FakeWorkspaces {
  _Stale(FakeWorkspaces base) : super(isLocal: base.isLocal);
  @override
  Future<WorkspaceResolution> set(WorkspaceTarget t, String path) async =>
      throw const WorkspaceRefused('This folder changed since you chose it.');
}
