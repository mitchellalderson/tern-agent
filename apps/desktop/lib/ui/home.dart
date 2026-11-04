import 'dart:io';
import 'dart:ui' show AppExitResponse;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/connection.dart';
import '../state/daemon.dart';
import '../state/store.dart';
import '../theme.dart';
import 'auth_card.dart';
import 'bundles.dart';
import 'channel_view.dart';
import 'config_editor.dart';
import 'connections.dart';
import 'conversation.dart';
import 'ledger.dart';
import 'mcp.dart';
import 'offline.dart';
import 'providers.dart';
import 'quit.dart';
import 'routines.dart';
import 'secrets.dart';
import 'settings.dart';
import 'side_panel.dart';
import 'sidebar.dart';
import 'tasks.dart';
import 'thread_pane.dart';
import 'where_to_run.dart';
import 'widgets.dart';

class Home extends StatefulWidget {
  const Home({super.key, required this.store, this.prefs, this.connections});

  final TernStore store;

  /// Loaded from disk when not given (tests).
  final AppPrefs? prefs;

  /// Where tern runs and switching it; without it the app stays on this
  /// computer and doesn't ask.
  final Connections? connections;

  @override
  State<Home> createState() => _HomeState();
}

class _HomeState extends State<Home> {
  var _panel = true;
  late final AppLifecycleListener _lifecycle;
  final _control = TernControl(bin: findTern());
  late final _prefs = widget.prefs ?? AppPrefs.load();

  /// On a tern server: nothing here to start, stop or start at login
  /// (spec §11.5, "Remote cores").
  bool get _remote =>
      widget.connections?.isRemote ??
      widget.store.client.endpoint is RemoteEndpoint;

  @override
  void initState() {
    super.initState();
    // Desktop windows report `resumed` while focused and `inactive` when not.
    _lifecycle = AppLifecycleListener(
      onStateChange: (s) =>
          widget.store.appActive = s == AppLifecycleState.resumed,
      onExitRequested: _onExit,
    );
    final connections = widget.connections;
    if (connections != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) maybeAskWhereToRun(context, _control, connections);
      });
    }
  }

  /// Closing the window quits the app; Settings decides whether tern
  /// stops with it.
  Future<AppExitResponse> _onExit() async {
    if (_remote || _prefs.onClose != OnClose.quitTern) {
      return AppExitResponse.exit;
    }
    final quit = await quitTernCompletely(
      context,
      widget.store,
      _control,
      exitApp: () async {},
    );
    return quit ? AppExitResponse.exit : AppExitResponse.cancel;
  }

  void _quitCompletely() => quitTernCompletely(context, widget.store, _control);

  bool get _canRestart => !_remote && _control.bin != null;

  /// For settings that apply at a restart; the app reconnects by itself.
  Future<void> _restartTern() async {
    await _control.stop();
    await _control.start();
  }

  void _settings() => showSettingsDialog(
    context,
    _control,
    _prefs,
    store: widget.store,
    connections: widget.connections,
    onRestart: _canRestart ? _restartTern : null,
  );

  @override
  void dispose() {
    _lifecycle.dispose();
    super.dispose();
  }

  void _togglePanel() => setState(() => _panel = !_panel);

  @override
  Widget build(BuildContext context) {
    final store = widget.store;
    final body = Scaffold(
      body: Stack(
        fit: StackFit.expand,
        children: [
          const Positioned(
            top: 0,
            left: 0,
            right: 0,
            child: Stripes(thickness: 2),
          ),
          Positioned.fill(
            top: 6,
            child: ListenableBuilder(
              listenable: store,
              builder: (context, _) {
                if (store.health == null) {
                  return OfflineView(store: store, onSettings: _settings);
                }
                return LayoutBuilder(
                  builder: (context, box) {
                    // Narrow windows drop the panel before the conversation.
                    final panel = _panel && box.maxWidth > 980;
                    return Row(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        SizedBox(width: 260, child: Sidebar(store: store)),
                        const Hairline(vertical: true),
                        Expanded(
                          child:
                              store.page == LibraryPage.connections &&
                                  store.canManageConnections
                              ? ConnectionsView(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                )
                              : store.page == LibraryPage.tasks &&
                                    store.hasTasks
                              ? TasksView(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                )
                              : store.page == LibraryPage.ledger &&
                                    store.hasLedger
                              ? LedgerView(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                )
                              : store.page == LibraryPage.routines
                              ? RoutinesPage(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                )
                              : store.page == LibraryPage.bundles &&
                                    store.canManageBundles
                              ? BundlesView(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                )
                              : store.page == LibraryPage.mcp && store.hasMcp
                              ? McpView(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                )
                              : store.topicOpen
                              ? ChannelView(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                )
                              : ConversationView(
                                  store: store,
                                  panelOpen: panel,
                                  onTogglePanel: _togglePanel,
                                ),
                        ),
                        // A conversation to read (a History entry, a moved one, a
                        // post's run) takes the panel's place while it's open.
                        if (store.thread != null) ...[
                          const Hairline(vertical: true),
                          SizedBox(width: 380, child: ThreadPane(store: store)),
                        ] else if (panel) ...[
                          const Hairline(vertical: true),
                          SizedBox(
                            width: 290,
                            child: SidePanel(
                              store: store,
                              onClose: _togglePanel,
                            ),
                          ),
                        ],
                      ],
                    );
                  },
                );
              },
            ),
          ),
          AuthCardOverlay(store: store),
        ],
      ),
    );
    // Native menus exist only on macOS; elsewhere the same shortcuts use Ctrl.
    if (!Platform.isMacOS) {
      return CallbackShortcuts(
        bindings: {
          const SingleActivator(LogicalKeyboardKey.keyN, control: true):
              store.newConversation,
          const SingleActivator(
            LogicalKeyboardKey.keyI,
            control: true,
            alt: true,
          ): _togglePanel,
          const SingleActivator(LogicalKeyboardKey.comma, control: true): () {
            if (store.health != null) showProvidersDialog(context, store);
          },
          if (!_remote)
            const SingleActivator(
              LogicalKeyboardKey.keyQ,
              control: true,
              alt: true,
            ): _quitCompletely,
        },
        child: Focus(autofocus: true, child: body),
      );
    }
    return PlatformMenuBar(
      menus: [
        PlatformMenu(
          label: 'tern',
          menus: [
            const PlatformMenuItemGroup(
              members: [
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.about,
                ),
              ],
            ),
            PlatformMenuItemGroup(
              members: [
                PlatformMenuItem(
                  label: 'Models…',
                  shortcut: const SingleActivator(
                    LogicalKeyboardKey.comma,
                    meta: true,
                  ),
                  onSelected: store.health == null
                      ? null
                      : () => showProvidersDialog(context, store),
                ),
                PlatformMenuItem(
                  label: 'Secrets…',
                  onSelected: store.health == null
                      ? null
                      : () => showSecretsDialog(context, store),
                ),
                PlatformMenuItem(
                  label: 'Configuration…',
                  onSelected: store.health == null
                      ? null
                      : () => showConfigDialog(
                          context,
                          store,
                          onRestart: _canRestart ? _restartTern : null,
                        ),
                ),
                PlatformMenuItem(label: 'Settings…', onSelected: _settings),
              ],
            ),
            const PlatformMenuItemGroup(
              members: [
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.hide,
                ),
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.hideOtherApplications,
                ),
              ],
            ),
            PlatformMenuItemGroup(
              members: [
                const PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.quit,
                ),
                // Stops tern too (spec §11.5a); ⌘Q only closes the window.
                // A server's tern isn't the app's to stop.
                if (!_remote)
                  PlatformMenuItem(
                    label: 'Quit tern completely',
                    shortcut: const SingleActivator(
                      LogicalKeyboardKey.keyQ,
                      meta: true,
                      alt: true,
                    ),
                    onSelected: _quitCompletely,
                  ),
              ],
            ),
          ],
        ),
        PlatformMenu(
          label: 'File',
          menus: [
            PlatformMenuItem(
              label: store.hasChannels ? 'New Topic' : 'New Conversation',
              shortcut: const SingleActivator(
                LogicalKeyboardKey.keyN,
                meta: true,
              ),
              onSelected: store.newConversation,
            ),
          ],
        ),
        PlatformMenu(
          label: 'View',
          menus: [
            PlatformMenuItem(
              label: 'Toggle Activity',
              shortcut: const SingleActivator(
                LogicalKeyboardKey.keyI,
                meta: true,
                alt: true,
              ),
              onSelected: _togglePanel,
            ),
          ],
        ),
        const PlatformMenu(
          label: 'Window',
          menus: [
            PlatformProvidedMenuItem(
              type: PlatformProvidedMenuItemType.minimizeWindow,
            ),
            PlatformProvidedMenuItem(
              type: PlatformProvidedMenuItemType.zoomWindow,
            ),
          ],
        ),
      ],
      child: body,
    );
  }
}
