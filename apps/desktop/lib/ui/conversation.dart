import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'items.dart';
import 'model_picker.dart';
import 'providers.dart';
import 'widgets.dart';

class ConversationView extends StatefulWidget {
  const ConversationView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  State<ConversationView> createState() => _ConversationViewState();
}

class _ConversationViewState extends State<ConversationView> {
  final _scroll = ScrollController();

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final store = widget.store;
    final t = store.current;
    final items = t?.items ?? const <Item>[];
    final phase = t?.phase;
    // The activity line sits below the newest item (index 0, reversed).
    final extra = phase == null ? 0 : 1;
    final title =
        store.sessions
            .where((s) => s.id == store.selected)
            .firstOrNull
            ?.title ??
        'New conversation';
    return Column(
      children: [
        _Header(
          title: title,
          store: store,
          panelOpen: widget.panelOpen,
          onTogglePanel: widget.onTogglePanel,
        ),
        const Hairline(),
        Expanded(
          child: items.isEmpty && store.setupNeeded
              ? _Welcome(store: store)
              : items.isEmpty
              ? const _Empty()
              // Reversed, so the newest content stays pinned to the bottom
              // while it streams.
              : ListView.builder(
                  controller: _scroll,
                  reverse: true,
                  padding: const EdgeInsets.symmetric(vertical: 20),
                  itemCount: items.length + extra,
                  itemBuilder: (context, i) {
                    final item = i < extra
                        ? null
                        : items[items.length - 1 - (i - extra)];
                    return Center(
                      child: ConstrainedBox(
                        constraints: const BoxConstraints(maxWidth: 760),
                        child: Padding(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 24,
                            vertical: 7,
                          ),
                          child: SizedBox(
                            width: double.infinity,
                            child: item == null
                                ? ActivityLine(
                                    phase: phase!,
                                    since: t!.runSince,
                                  )
                                : ItemView(item: item, store: store),
                          ),
                        ),
                      ),
                    );
                  },
                ),
        ),
        if (store.error case final e?)
          _ErrorBanner(
            message: e,
            onClose: () {
              store.error = null;
              setState(() {});
            },
          ),
        _Composer(store: store),
      ],
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({
    required this.title,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final String title;
  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final model = store.modelFor(store.selected);
    return WindowDragArea(
      child: Container(
        height: titleBarInset + 12,
        padding: const EdgeInsets.fromLTRB(20, 0, 10, 0),
        alignment: Alignment.center,
        child: Row(
          children: [
            const AgentAvatar(name: 'tern', size: 24),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(
                  fontSize: 15,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ),
            if (store.modelWarning case final w?)
              Tooltip(
                message: w,
                child: Icon(Icons.info_outline, size: 15, color: c.warn),
              ),
            if (store.link == Link.online)
              TextButton.icon(
                onPressed: () => model == null
                    ? showProvidersDialog(context, store)
                    : _pickModel(context),
                icon: Icon(Icons.memory, size: 15, color: c.muted),
                label: Text(
                  model ?? 'Set up a model',
                  style: TextStyle(
                    fontSize: 12.5,
                    color: model == null ? c.warn : c.muted,
                  ),
                ),
                style: TextButton.styleFrom(
                  shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(8),
                  ),
                ),
              ),
            QuietIconButton(
              icon: panelOpen
                  ? Icons.view_sidebar
                  : Icons.view_sidebar_outlined,
              tooltip: panelOpen ? 'Hide activity' : 'Show activity',
              onPressed: onTogglePanel,
            ),
          ],
        ),
      ),
    );
  }

  Future<void> _pickModel(BuildContext context) {
    if (store.modelLists == null && !store.loadingModels) store.loadModels();
    return showDialog<void>(
      context: context,
      builder: (dialog) => ModelPickerDialog(
        store: store,
        onManage: () => showProvidersDialog(context, store),
      ),
    );
  }
}

/// First run: no usable model yet, so set one up right here.
class _Welcome extends StatelessWidget {
  const _Welcome({required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return SingleChildScrollView(
      padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 32),
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 520),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Center(child: AgentAvatar(name: 'tern', size: 48)),
              const SizedBox(height: 16),
              Text(
                'Connect a model to get started',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.titleMedium
                    ?.copyWith(fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 6),
              Text(
                'tern needs a model provider and its API key. The key stays '
                'on this computer, in ~/.tern/secrets.toml.',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 20),
              Container(
                padding: const EdgeInsets.all(18),
                decoration: BoxDecoration(
                  color: c.card,
                  border: Border.all(color: c.hairline),
                  borderRadius: BorderRadius.circular(12),
                ),
                child: ProviderSetup(
                  store: store,
                  onDone: (_) => store.refreshInfo(),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty();

  @override
  Widget build(BuildContext context) => Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const AgentAvatar(name: 'tern', size: 48),
        const SizedBox(height: 16),
        Text(
          'What should tern do?',
          style: Theme.of(context).textTheme.titleMedium
              ?.copyWith(fontWeight: FontWeight.w600),
        ),
        const SizedBox(height: 6),
        Text(
          'Runs keep going in the background if you close this window.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ],
    ),
  );
}

class _ErrorBanner extends StatelessWidget {
  const _ErrorBanner({required this.message, required this.onClose});

  final String message;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      margin: const EdgeInsets.fromLTRB(24, 0, 24, 8),
      constraints: const BoxConstraints(maxWidth: 712),
      padding: const EdgeInsets.fromLTRB(12, 4, 4, 4),
      decoration: BoxDecoration(
        color: c.error.withValues(alpha: 0.08),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Icon(Icons.error_outline, size: 15, color: c.error),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              message,
              style: TextStyle(fontSize: 12.5, color: c.error),
            ),
          ),
          QuietIconButton(
            icon: Icons.close,
            tooltip: 'Dismiss',
            onPressed: onClose,
            size: 15,
          ),
        ],
      ),
    );
  }
}

class _Composer extends StatefulWidget {
  const _Composer({required this.store});

  final TernStore store;

  @override
  State<_Composer> createState() => _ComposerState();
}

class _ComposerState extends State<_Composer> {
  final _text = TextEditingController();
  final _focus = FocusNode();

  @override
  void initState() {
    super.initState();
    _text.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _text.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _send() {
    final text = _text.text.trim();
    if (text.isEmpty || widget.store.link != Link.online) return;
    _text.clear();
    widget.store.send(text);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final store = widget.store;
    final running = store.running;
    final canSend = _text.text.trim().isNotEmpty && store.link == Link.online;
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 4, 24, 18),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 712),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (running && _text.text.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(left: 14, bottom: 6),
                child: Text(
                  'Sending will stop the current run.',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
            Container(
              decoration: BoxDecoration(
                color: c.background,
                border: Border.all(
                  color: _focus.hasFocus ? c.muted : c.hairline,
                ),
                borderRadius: BorderRadius.circular(22),
                boxShadow: [
                  BoxShadow(
                    color: Colors.black.withValues(alpha: 0.04),
                    blurRadius: 12,
                    offset: const Offset(0, 2),
                  ),
                ],
              ),
              padding: const EdgeInsets.fromLTRB(18, 4, 6, 4),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.end,
                children: [
                  Expanded(
                    child: Focus(
                      onFocusChange: (_) => setState(() {}),
                      // ⏎ sends; ⇧⏎ inserts a newline.
                      onKeyEvent: (node, e) {
                        if (e is KeyDownEvent &&
                            e.logicalKey == LogicalKeyboardKey.enter &&
                            !HardwareKeyboard.instance.isShiftPressed) {
                          _send();
                          return KeyEventResult.handled;
                        }
                        return KeyEventResult.ignored;
                      },
                      child: TextField(
                        controller: _text,
                        focusNode: _focus,
                        autofocus: true,
                        minLines: 1,
                        maxLines: 8,
                        keyboardType: TextInputType.multiline,
                        decoration: InputDecoration(
                          hintText: 'Message tern',
                          hintStyle: TextStyle(color: c.muted),
                          border: InputBorder.none,
                          contentPadding: const EdgeInsets.symmetric(
                            vertical: 12,
                          ),
                        ),
                      ),
                    ),
                  ),
                  Padding(
                    padding: const EdgeInsets.only(bottom: 4),
                    child: running && !canSend
                        ? _RoundButton(
                            icon: Icons.stop_rounded,
                            tooltip: 'Stop',
                            onPressed: store.stop,
                            filled: false,
                          )
                        : _RoundButton(
                            icon: Icons.arrow_upward_rounded,
                            tooltip: 'Send (⏎)',
                            onPressed: canSend ? _send : null,
                            filled: true,
                          ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _RoundButton extends StatelessWidget {
  const _RoundButton({
    required this.icon,
    required this.tooltip,
    required this.onPressed,
    required this.filled,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;
  final bool filled;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final enabled = onPressed != null;
    return Tooltip(
      message: tooltip,
      child: Material(
        color: filled ? (enabled ? c.text : c.hairline) : c.selection,
        shape: const CircleBorder(),
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: onPressed,
          child: SizedBox(
            width: 34,
            height: 34,
            child: Icon(icon, size: 19, color: filled ? c.background : c.text),
          ),
        ),
      ),
    );
  }
}
