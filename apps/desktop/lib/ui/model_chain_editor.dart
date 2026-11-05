import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'model_picker.dart';

/// Whether `chain` can't be saved as it is (an entry with no provider, a
/// repeat, a bad format). A single model is left to the daemon, which
/// checks it on save and says why (a typed `provider:model` the lists don't
/// know stays a way in), and an entry already in [saved] isn't held against
/// the user: a provider removed since is flagged on its row, not a reason
/// to lose every other edit.
bool chainBlocked(
  TernStore store,
  List<String> chain, {
  List<String> saved = const [],
}) =>
    chain.length > 1 &&
    checkModelChain(chain, lists: store.modelLists).any(
      (i) =>
          i.blocking &&
          !(i.kind == ChainIssueKind.unknownProvider &&
              saved.contains(i.model)),
    );

/// When fallbacks come into play, in the words the editor shows. T6a:
/// retryable failures (overloaded, rate limited, server errors, network)
/// after the agent's usual retries, and a rejected key or empty balance at
/// once; not a bad request or a refusal.
const fallbackExplanation =
    'tern uses the first model. If it fails (overloaded, rate-limited, a '
    'server error, a lost connection, or a rejected key or empty balance), '
    'it tries the next one down. A request the model refuses or gets wrong '
    "doesn't move on.";

/// Pick one model from the app's searchable list in a dialog. Returns
/// `provider:model`, or null when dismissed.
Future<String?> showModelChoice(
  BuildContext context,
  TernStore store, {
  String? current,
  String title = 'Choose a model',
}) {
  if (store.modelLists == null && !store.loadingModels) store.loadModels();
  return showDialog<String>(
    context: context,
    builder: (_) =>
        _ModelChoiceDialog(store: store, current: current, title: title),
  );
}

class _ModelChoiceDialog extends StatelessWidget {
  const _ModelChoiceDialog({
    required this.store,
    required this.current,
    required this.title,
  });

  final TernStore store;
  final String? current;
  final String title;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 500),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 14, 16, 12),
          child: ListenableBuilder(
            listenable: store,
            builder: (context, _) => Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(2, 0, 2, 8),
                  child: Text(
                    title,
                    style: const TextStyle(
                      fontSize: 15,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
                if (store.daemonTooOld)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 6),
                    child: Text(
                      "This tern can't list models yet: type one as "
                      'provider:model.',
                      style: TextStyle(fontSize: 12.5, color: c.warn),
                    ),
                  ),
                ModelList(
                  lists: store.modelLists,
                  loading: store.loadingModels,
                  current: current,
                  maxHeight: 300,
                  onChoose: (m) => Navigator.of(context).pop(m),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The ordered list of models a setting uses: the first one is the
/// **primary**, the rest are **then try** fallbacks in order (T6a). Rows
/// reorder by dragging the handle, or with the up and down buttons (so the
/// keyboard and screen readers can too); each can be replaced (tap the
/// model) or removed; "Add a fallback" opens the app's searchable model
/// list. Each entry is checked against the providers' lists as it changes,
/// and problems are shown on the row in words.
///
/// It edits a copy: [onChanged] gets the whole new list, and the caller
/// decides when to save it (as `provider:model` or a list, the same value
/// config.toml holds).
class ModelChainEditor extends StatefulWidget {
  const ModelChainEditor({
    super.key,
    required this.store,
    required this.chain,
    required this.onChanged,
    this.showExplanation = true,
    this.enabled = true,
  });

  final TernStore store;
  final List<String> chain;
  final ValueChanged<List<String>> onChanged;

  /// The short "when do fallbacks kick in" note above the list.
  final bool showExplanation;
  final bool enabled;

  @override
  State<ModelChainEditor> createState() => _ModelChainEditorState();
}

class _ModelChainEditorState extends State<ModelChainEditor> {
  String? _notice;

  List<String> get _chain => widget.chain;

  void _set(List<String> next) {
    setState(() => _notice = null);
    widget.onChanged(next);
  }

  void _move(int from, int to) {
    if (to < 0 || to >= _chain.length || from == to) return;
    final next = [..._chain];
    next.insert(to, next.removeAt(from));
    _set(next);
  }

  void _remove(int i) {
    if (_chain.length <= 1) return;
    _set([..._chain]..removeAt(i));
  }

  Future<void> _replace(int i) async {
    final picked = await showModelChoice(
      context,
      widget.store,
      current: _chain[i],
      title: i == 0 ? 'Primary model' : 'Fallback model',
    );
    if (picked == null || picked == _chain[i]) return;
    if (_chain.contains(picked)) {
      setState(() => _notice = 'That model is already in the list.');
      return;
    }
    _set([..._chain]..[i] = picked);
  }

  Future<void> _add() async {
    final picked = await showModelChoice(
      context,
      widget.store,
      title: 'Add a fallback',
    );
    if (picked == null) return;
    if (_chain.contains(picked)) {
      setState(() => _notice = 'That model is already in the list.');
      return;
    }
    _set([..._chain, picked]);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    return ListenableBuilder(
      listenable: widget.store,
      builder: (context, _) {
        final issues = checkModelChain(_chain, lists: widget.store.modelLists);
        ChainIssue? issueAt(int i) =>
            issues.where((x) => x.index == i).firstOrNull;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (widget.showExplanation)
              Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Padding(
                      padding: const EdgeInsets.only(top: 1),
                      child: Icon(Icons.info_outline, size: 14, color: c.muted),
                    ),
                    const SizedBox(width: 6),
                    Expanded(
                      child: Text(
                        fallbackExplanation,
                        key: const Key('fallback-explanation'),
                        style: small,
                      ),
                    ),
                  ],
                ),
              ),
            ReorderableListView.builder(
              key: const Key('chain-list'),
              shrinkWrap: true,
              physics: const NeverScrollableScrollPhysics(),
              buildDefaultDragHandles: false,
              itemCount: _chain.length,
              onReorderItem: widget.enabled ? _move : (_, _) {},
              proxyDecorator: (child, _, _) => Material(
                color: Colors.transparent,
                elevation: 3,
                borderRadius: BorderRadius.circular(8),
                child: child,
              ),
              itemBuilder: (context, i) => _ChainRow(
                key: ValueKey('chain-$i-${_chain[i]}'),
                index: i,
                count: _chain.length,
                model: _chain[i],
                store: widget.store,
                issue: issueAt(i),
                enabled: widget.enabled,
                onMoveUp: () => _move(i, i - 1),
                onMoveDown: () => _move(i, i + 1),
                onRemove: () => _remove(i),
                onReplace: () => _replace(i),
              ),
            ),
            if (_notice case final n?)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(
                  n,
                  key: const Key('chain-notice'),
                  style: TextStyle(fontSize: 12, color: c.warn),
                ),
              ),
            Align(
              alignment: Alignment.centerLeft,
              child: TextButton.icon(
                key: const Key('add-fallback'),
                onPressed: widget.enabled ? _add : null,
                icon: Icon(Icons.add, size: 16, color: c.muted),
                label: Text('Add a fallback', style: TextStyle(color: c.muted)),
              ),
            ),
          ],
        );
      },
    );
  }
}

class _ChainRow extends StatelessWidget {
  const _ChainRow({
    super.key,
    required this.index,
    required this.count,
    required this.model,
    required this.store,
    required this.issue,
    required this.enabled,
    required this.onMoveUp,
    required this.onMoveDown,
    required this.onRemove,
    required this.onReplace,
  });

  final int index;
  final int count;
  final String model;
  final TernStore store;
  final ChainIssue? issue;
  final bool enabled;
  final VoidCallback onMoveUp;
  final VoidCallback onMoveDown;
  final VoidCallback onRemove;
  final VoidCallback onReplace;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final ref = parseModelRef(model);
    final info = ref == null
        ? null
        : store.modelLists
              ?.where((l) => l.provider == ref.provider)
              .expand((l) => l.models)
              .where((m) => m.id == ref.id)
              .firstOrNull;
    final primary = index == 0;
    final problem = issue;
    final edge = problem == null
        ? c.hairline
        : problem.blocking
        ? c.error
        : c.warn;
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: primary ? c.selection.withValues(alpha: 0.5) : null,
          border: Border.all(color: edge),
          borderRadius: BorderRadius.circular(8),
        ),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(2, 4, 4, 4),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  ReorderableDragStartListener(
                    index: index,
                    enabled: enabled && count > 1,
                    child: Tooltip(
                      message: 'Drag to reorder',
                      child: MouseRegion(
                        cursor: enabled && count > 1
                            ? SystemMouseCursors.grab
                            : MouseCursor.defer,
                        child: Padding(
                          padding: const EdgeInsets.all(6),
                          child: Icon(
                            Icons.drag_indicator,
                            size: 18,
                            color: count > 1 ? c.muted : c.hairline,
                          ),
                        ),
                      ),
                    ),
                  ),
                  SizedBox(
                    width: 62,
                    child: Text(
                      primary ? 'PRIMARY' : 'THEN TRY',
                      key: Key('chain-label-$index'),
                      style: TextStyle(
                        fontSize: 10,
                        letterSpacing: 0.6,
                        fontWeight: FontWeight.w700,
                        color: primary ? c.ink : c.muted,
                      ),
                    ),
                  ),
                  Expanded(
                    child: InkWell(
                      key: Key('chain-pick-$index'),
                      borderRadius: BorderRadius.circular(6),
                      onTap: enabled ? onReplace : null,
                      child: Padding(
                        padding: const EdgeInsets.symmetric(
                          vertical: 3,
                          horizontal: 4,
                        ),
                        child: Row(
                          children: [
                            if (ref != null) ...[
                              ProviderBadge(store.providerLabel(ref.provider)),
                              const SizedBox(width: 8),
                            ],
                            Expanded(
                              child: Text.rich(
                                TextSpan(
                                  children: [
                                    TextSpan(
                                      text: ref?.id ?? model,
                                      style: mono.copyWith(fontSize: 12.5),
                                    ),
                                    if (info?.name case final n?
                                        when n != ref?.id)
                                      TextSpan(
                                        text: '   $n',
                                        style: TextStyle(
                                          fontSize: 11.5,
                                          color: c.muted,
                                        ),
                                      ),
                                  ],
                                ),
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
                  QuietIconButtonLite(
                    key: Key('chain-up-$index'),
                    icon: Icons.arrow_upward,
                    tooltip: 'Move up',
                    onPressed: enabled && index > 0 ? onMoveUp : null,
                  ),
                  QuietIconButtonLite(
                    key: Key('chain-down-$index'),
                    icon: Icons.arrow_downward,
                    tooltip: 'Move down',
                    onPressed: enabled && index < count - 1 ? onMoveDown : null,
                  ),
                  QuietIconButtonLite(
                    key: Key('chain-remove-$index'),
                    icon: Icons.close,
                    tooltip: count > 1
                        ? 'Remove from the list'
                        : "A list needs at least one model",
                    onPressed: enabled && count > 1 ? onRemove : null,
                  ),
                ],
              ),
              if (problem case final p?)
                Padding(
                  padding: const EdgeInsets.fromLTRB(32, 0, 8, 4),
                  child: Text(
                    p.message,
                    key: Key('chain-issue-$index'),
                    style: TextStyle(
                      fontSize: 11.5,
                      color: p.blocking ? c.error : c.warn,
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// A small icon button for dense rows (a 28 px target; the tooltip names
/// it for screen readers).
class QuietIconButtonLite extends StatelessWidget {
  const QuietIconButtonLite({
    super.key,
    required this.icon,
    required this.tooltip,
    required this.onPressed,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) => SizedBox.square(
    dimension: 28,
    child: IconButton(
      padding: EdgeInsets.zero,
      icon: Icon(icon, size: 16),
      tooltip: tooltip,
      onPressed: onPressed,
      color: context.colors.muted,
      disabledColor: context.colors.hairline,
      splashRadius: 14,
    ),
  );
}
