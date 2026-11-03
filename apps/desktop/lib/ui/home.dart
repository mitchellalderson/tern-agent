import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../state/store.dart';
import 'conversation.dart';
import 'offline.dart';
import 'side_panel.dart';
import 'sidebar.dart';
import 'widgets.dart';

class Home extends StatefulWidget {
  const Home({super.key, required this.store});

  final TernStore store;

  @override
  State<Home> createState() => _HomeState();
}

class _HomeState extends State<Home> {
  var _panel = true;
  late final AppLifecycleListener _lifecycle;

  @override
  void initState() {
    super.initState();
    // Desktop windows report `resumed` while focused and `inactive` when not.
    _lifecycle = AppLifecycleListener(
      onStateChange: (s) =>
          widget.store.appActive = s == AppLifecycleState.resumed,
    );
  }

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
      body: ListenableBuilder(
        listenable: store,
        builder: (context, _) {
          if (store.health == null) return OfflineView(store: store);
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
                    child: ConversationView(
                      store: store,
                      panelOpen: panel,
                      onTogglePanel: _togglePanel,
                    ),
                  ),
                  if (panel) ...[
                    const Hairline(vertical: true),
                    SizedBox(
                      width: 290,
                      child: SidePanel(store: store, onClose: _togglePanel),
                    ),
                  ],
                ],
              );
            },
          );
        },
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
        },
        child: Focus(autofocus: true, child: body),
      );
    }
    return PlatformMenuBar(
      menus: [
        const PlatformMenu(
          label: 'tern',
          menus: [
            PlatformMenuItemGroup(
              members: [
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.about,
                ),
              ],
            ),
            PlatformMenuItemGroup(
              members: [
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.hide,
                ),
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.hideOtherApplications,
                ),
              ],
            ),
            PlatformProvidedMenuItem(type: PlatformProvidedMenuItemType.quit),
          ],
        ),
        PlatformMenu(
          label: 'File',
          menus: [
            PlatformMenuItem(
              label: 'New Conversation',
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
