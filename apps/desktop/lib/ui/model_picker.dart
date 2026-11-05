import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

/// A line of the model list.
sealed class ModelRow {
  const ModelRow();
}

class ModelHeader extends ModelRow {
  const ModelHeader(this.title, [this.note]);
  final String title;

  /// Why the list is missing or stale.
  final String? note;
}

class ModelChoice extends ModelRow {
  const ModelChoice(
    this.full,
    this.info, {
    this.current = false,
    this.providerLabel,
    this.suggested = false,
  });

  /// `provider:id`.
  final String full;
  final ModelInfo? info;
  final bool current;

  /// The provider's display name, shown as a badge.
  final String? providerLabel;

  /// From a preset's short list, not the provider's own (the list couldn't
  /// be fetched).
  final bool suggested;

  String get id => full.substring(full.indexOf(':') + 1);
}

/// Use the filter text as typed.
class TypedChoice extends ModelRow {
  const TypedChoice(this.text);
  final String text;
}

/// The rows for `lists` filtered by `filter`: the current model first, then
/// each provider's matches, then the typed text (docs/provider-setup-spec.md
/// §7.2). Pure, for tests.
List<ModelRow> modelRows(
  List<ProviderModels> lists,
  String filter, {
  String? current,
}) {
  final f = filter.trim();
  final rows = <ModelRow>[];
  if (f.isEmpty && current != null) {
    final colon = current.indexOf(':');
    final provider = colon < 0 ? '' : current.substring(0, colon);
    final id = current.substring(colon + 1);
    final info = lists
        .where((l) => l.provider == provider)
        .expand((l) => l.models)
        .where((m) => m.id == id)
        .firstOrNull;
    rows
      ..add(const ModelHeader('Current'))
      ..add(
        ModelChoice(
          current,
          info,
          current: true,
          providerLabel: lists
              .where((l) => l.provider == provider)
              .firstOrNull
              ?.label,
        ),
      );
  }
  for (final l in lists) {
    final models = [
      for (final m in l.models)
        if (f.isEmpty || _matches(f, l.provider, m)) m,
    ];
    // With no list, a preset's few suggestions stand in, clearly labelled.
    final suggested = [
      if (l.models.isEmpty)
        for (final s in l.suggested)
          if (f.isEmpty || _subsequence(f, s)) s,
    ];
    if (models.isEmpty && suggested.isEmpty && f.isNotEmpty) continue;
    final note = switch ((l.message, l.models.isEmpty)) {
      (final m?, true) => "Couldn't list models: $m",
      (final m?, false) => 'Showing the last list: $m',
      (null, true) when l.suggested.isNotEmpty =>
        'No list available. Suggestions below, or enter a model id',
      (null, true) => 'No models listed; type a model id',
      (null, false) => null,
    };
    rows.add(ModelHeader(l.label, note));
    for (final m in models) {
      final full = '${l.provider}:${m.id}';
      rows.add(
        ModelChoice(full, m, current: full == current, providerLabel: l.label),
      );
    }
    for (final s in suggested) {
      final full = '${l.provider}:$s';
      rows.add(
        ModelChoice(
          full,
          null,
          current: full == current,
          providerLabel: l.label,
          suggested: true,
        ),
      );
    }
  }
  if (f.isNotEmpty) rows.add(TypedChoice(f));
  return rows;
}

/// The provider counts only when the filter names one (`ollama:`), so
/// "opus" doesn't match every Anthropic model through "anthropic".
bool _matches(String filter, String provider, ModelInfo m) =>
    filter.contains(':')
    ? _subsequence(filter, '$provider:${m.id}')
    : _subsequence(filter, m.id) ||
          (m.name != null && _subsequence(filter, m.name!));

bool _subsequence(String needle, String hay) {
  final h = hay.toLowerCase();
  var i = 0;
  for (final c in needle.toLowerCase().split('')) {
    i = h.indexOf(c, i);
    if (i < 0) return false;
    i++;
  }
  return true;
}

String? _choiceOf(ModelRow r) => switch (r) {
  ModelChoice(:final full) => full,
  TypedChoice(:final text) => text,
  ModelHeader() => null,
};

/// A searchable list of models: type to filter, ↑↓ to move, ⏎ to choose.
/// The typed text is always a choice, so unlisted models work too.
class ModelList extends StatefulWidget {
  const ModelList({
    super.key,
    required this.lists,
    required this.onChoose,
    this.current,
    this.loading = false,
    this.maxHeight = 340,
    this.autofocus = true,
  });

  final List<ProviderModels>? lists;
  final String? current;
  final bool loading;
  final double maxHeight;
  final bool autofocus;
  final ValueChanged<String> onChoose;

  @override
  State<ModelList> createState() => ModelListState();
}

class ModelListState extends State<ModelList> {
  final _filter = TextEditingController();
  final _scroll = ScrollController();
  var _selected = 0;

  /// "Enter a model id": the list is hidden and the box takes an id as is.
  var _manual = false;
  String? _manualProvider;

  @override
  void dispose() {
    _filter.dispose();
    _scroll.dispose();
    super.dispose();
  }

  List<ModelRow> get _rows => modelRows(
    widget.lists ?? const [],
    _filter.text,
    current: widget.current,
  );

  /// The highlighted choice, if any.
  String? get chosen {
    final choices = _rows.map(_choiceOf).nonNulls.toList();
    return choices.isEmpty
        ? null
        : choices[_selected.clamp(0, choices.length - 1)];
  }

  void _move(int by) {
    final n = _rows.map(_choiceOf).nonNulls.length;
    if (n == 0) return;
    setState(() => _selected = (_selected + by).clamp(0, n - 1));
  }

  /// The provider a typed id belongs to: the one picked, else the current
  /// model's, else the first.
  String? get _provider {
    final lists = widget.lists ?? const <ProviderModels>[];
    if (lists.isEmpty) return null;
    final cur = widget.current;
    final fromCurrent = cur == null || !cur.contains(':')
        ? null
        : cur.substring(0, cur.indexOf(':'));
    final want = _manualProvider ?? fromCurrent;
    return lists.any((l) => l.provider == want) ? want : lists.first.provider;
  }

  void _useTyped() {
    final id = _filter.text.trim();
    if (id.isEmpty) return;
    final p = _provider;
    widget.onChoose(p == null ? id : '$p:$id');
  }

  KeyEventResult _onKey(FocusNode _, KeyEvent e) {
    if (e is! KeyDownEvent && e is! KeyRepeatEvent) {
      return KeyEventResult.ignored;
    }
    switch (e.logicalKey) {
      case LogicalKeyboardKey.arrowDown:
        _move(1);
      case LogicalKeyboardKey.arrowUp:
        _move(-1);
      case LogicalKeyboardKey.enter || LogicalKeyboardKey.numpadEnter:
        if (_manual) {
          _useTyped();
        } else if (chosen case final c?) {
          widget.onChoose(c);
        }
      default:
        return KeyEventResult.ignored;
    }
    return KeyEventResult.handled;
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final rows = _rows;
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Focus(
          onKeyEvent: _onKey,
          child: TextField(
            key: const ValueKey('model-filter'),
            controller: _filter,
            autofocus: widget.autofocus,
            autocorrect: false,
            enableSuggestions: false,
            style: mono.copyWith(fontSize: 13),
            onChanged: (_) => setState(() => _selected = 0),
            decoration: InputDecoration(
              hintText: _manual
                  ? 'The model id, e.g. vendor/model-name'
                  : 'Search models',
              hintStyle: TextStyle(color: c.muted),
              prefixIcon: Icon(Icons.search, size: 17, color: c.muted),
              isDense: true,
              contentPadding: const EdgeInsets.all(10),
              border: OutlineInputBorder(
                borderRadius: BorderRadius.circular(8),
                borderSide: BorderSide(color: c.hairline),
              ),
              enabledBorder: OutlineInputBorder(
                borderRadius: BorderRadius.circular(8),
                borderSide: BorderSide(color: c.hairline),
              ),
              focusedBorder: OutlineInputBorder(
                borderRadius: BorderRadius.circular(8),
                borderSide: BorderSide(color: c.ink),
              ),
            ),
          ),
        ),
        const SizedBox(height: 8),
        if (_manual) ..._manualBody(context) else ..._listBody(context, rows),
      ],
    );
  }

  List<Widget> _manualBody(BuildContext context) {
    final c = context.colors;
    final lists = widget.lists ?? const <ProviderModels>[];
    final id = _filter.text.trim();
    return [
      Text(
        "Type the model's id exactly as the provider names it, for example "
        'vendor/model-name. tern adds the provider for you.',
        style: TextStyle(fontSize: 12, color: c.muted),
      ),
      if (lists.length > 1) ...[
        const SizedBox(height: 8),
        Row(
          children: [
            Text('Provider', style: TextStyle(fontSize: 12, color: c.muted)),
            const SizedBox(width: 10),
            DropdownButton<String>(
              key: const ValueKey('manual-provider'),
              value: _provider,
              isDense: true,
              underline: const SizedBox.shrink(),
              style: TextStyle(fontSize: 13, color: c.ink),
              items: [
                for (final l in lists)
                  DropdownMenuItem(value: l.provider, child: Text(l.label)),
              ],
              onChanged: (v) => setState(() => _manualProvider = v),
            ),
          ],
        ),
      ],
      const SizedBox(height: 8),
      Row(
        children: [
          TextButton(
            onPressed: () => setState(() => _manual = false),
            child: const Text('Back to the list'),
          ),
          const Spacer(),
          FilledButton(
            key: const ValueKey('use-typed-model'),
            style: FilledButton.styleFrom(
              backgroundColor: c.accent,
              foregroundColor: c.onAccent,
            ),
            onPressed: id.isEmpty ? null : _useTyped,
            child: const Text('Use this model'),
          ),
        ],
      ),
    ];
  }

  List<Widget> _listBody(BuildContext context, List<ModelRow> rows) {
    final c = context.colors;
    var choice = -1;
    final grouped = (widget.lists?.length ?? 0) > 1;
    return [
      ConstrainedBox(
        constraints: BoxConstraints(maxHeight: widget.maxHeight),
        child: widget.lists == null && widget.loading
            ? const Padding(
                padding: EdgeInsets.all(20),
                child: Center(
                  child: SizedBox.square(
                    dimension: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                ),
              )
            : rows.isEmpty
            ? Padding(
                padding: const EdgeInsets.all(14),
                child: Text(
                  'No models listed. Type a model id.',
                  style: TextStyle(fontSize: 12.5, color: c.muted),
                ),
              )
            : ListView(
                controller: _scroll,
                shrinkWrap: true,
                children: [
                  for (final r in rows)
                    switch (r) {
                      // One provider with nothing to say needs no heading:
                      // each row carries its provider as a badge.
                      ModelHeader(:final title, :final note)
                          when !grouped && note == null && title != 'Current' =>
                        const SizedBox.shrink(),
                      ModelHeader(:final title, :final note) => _Header(
                        title: title,
                        note: note,
                      ),
                      _ => () {
                        choice++;
                        final i = choice;
                        return _Choice(
                          row: r,
                          selected: i == _selected,
                          onTap: () => widget.onChoose(_choiceOf(r)!),
                          onHover: () => setState(() => _selected = i),
                        );
                      }(),
                    },
                ],
              ),
      ),
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton(
          key: const ValueKey('enter-model-id'),
          onPressed: () => setState(() {
            _manual = true;
            _filter.clear();
            _selected = 0;
          }),
          child: const Text('Enter a model id'),
        ),
      ),
    ];
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.title, this.note});

  final String title;
  final String? note;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 10, 8, 4),
      child: Row(
        children: [
          Text(
            title,
            style: TextStyle(
              fontSize: 11.5,
              fontWeight: FontWeight.w600,
              color: c.muted,
            ),
          ),
          if (note case final n?) ...[
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                n,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 11.5, color: c.warn),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class _Choice extends StatelessWidget {
  const _Choice({
    required this.row,
    required this.selected,
    required this.onTap,
    required this.onHover,
  });

  final ModelRow row;
  final bool selected;
  final VoidCallback onTap;
  final VoidCallback onHover;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final (label, note, current) = switch (row) {
      ModelChoice(:final id, :final info, :final current, :final suggested) => (
        id,
        [
          if (suggested) 'suggested',
          if (info?.name case final n? when n != id) n,
          if (info?.contextWindow case final w?) '${w ~/ 1000}k context',
        ].join(' · '),
        current,
      ),
      TypedChoice(:final text) => ('Use "$text"', 'as typed', false),
      ModelHeader() => ('', '', false),
    };
    final badge = switch (row) {
      ModelChoice(:final providerLabel) => providerLabel,
      _ => null,
    };
    return MouseRegion(
      onEnter: (_) => onHover(),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(6),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
          decoration: BoxDecoration(
            color: selected ? c.selection : null,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            children: [
              // One text, so the id and its note share the row and the
              // check mark sits at the end.
              Expanded(
                child: Text.rich(
                  TextSpan(
                    children: [
                      TextSpan(
                        text: label,
                        style: row is TypedChoice
                            ? const TextStyle(fontSize: 13)
                            : mono.copyWith(fontSize: 13),
                      ),
                      if (note.isNotEmpty)
                        TextSpan(
                          text: '   $note',
                          style: TextStyle(fontSize: 11.5, color: c.muted),
                        ),
                    ],
                  ),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              if (badge != null) ...[
                const SizedBox(width: 8),
                ProviderBadge(badge),
              ],
              if (current) ...[
                const SizedBox(width: 6),
                Icon(Icons.check, size: 15, color: c.accent),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// A provider's name as a small pill, next to a model.
class ProviderBadge extends StatelessWidget {
  const ProviderBadge(this.label, {super.key});

  final String label;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
      decoration: BoxDecoration(
        color: c.selection,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Text(label, style: TextStyle(fontSize: 10.5, color: c.muted)),
    );
  }
}

/// The conversation's model chip opens this.
Future<void> showModelPicker(BuildContext context, TernStore store) {
  if (store.modelLists == null && !store.loadingModels) {
    store.loadModels();
  }
  return showDialog<void>(
    context: context,
    builder: (_) => ModelPickerDialog(store: store),
  );
}

class ModelPickerDialog extends StatefulWidget {
  const ModelPickerDialog({super.key, required this.store, this.onManage});

  final TernStore store;

  /// Opens provider settings; defaults to doing nothing when absent.
  final VoidCallback? onManage;

  @override
  State<ModelPickerDialog> createState() => _ModelPickerDialogState();
}

class _ModelPickerDialogState extends State<ModelPickerDialog> {
  var _makeDefault = false;
  var _busy = false;
  String? _error;

  Future<void> _choose(String model) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.store.setModel(model, makeDefault: _makeDefault);
    if (!mounted) return;
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final store = widget.store;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(18, 16, 18, 10),
          child: ListenableBuilder(
            listenable: store,
            builder: (context, _) => Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    const Expanded(
                      child: Text(
                        'Model',
                        style: TextStyle(
                          fontSize: 16,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ),
                    IconButton(
                      tooltip: 'Ask the providers again',
                      icon: store.loadingModels
                          ? const SizedBox.square(
                              dimension: 14,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            )
                          : Icon(Icons.refresh, size: 17, color: c.muted),
                      onPressed: store.loadingModels
                          ? null
                          : () => store.loadModels(refresh: true),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                if (store.daemonTooOld) ...[
                  Text(
                    '${TernStore.daemonTooOldMessage} Until then, type a '
                    'model as provider:model.',
                    style: TextStyle(fontSize: 12.5, color: c.warn),
                  ),
                  const SizedBox(height: 8),
                ],
                ModelList(
                  lists: store.modelLists,
                  loading: store.loadingModels,
                  current: store.modelFor(store.selected),
                  onChoose: _busy ? (_) {} : _choose,
                ),
                if (_error case final e?) ...[
                  const SizedBox(height: 8),
                  Text(e, style: TextStyle(fontSize: 12.5, color: c.error)),
                ],
                const SizedBox(height: 6),
                Row(
                  children: [
                    Checkbox(
                      value: _makeDefault,
                      visualDensity: VisualDensity.compact,
                      onChanged: (v) => setState(() => _makeDefault = v!),
                    ),
                    Text(
                      'Also make it the default',
                      style: TextStyle(fontSize: 12.5, color: c.muted),
                    ),
                    const Spacer(),
                    if (widget.onManage case final manage?)
                      TextButton(
                        onPressed: () {
                          Navigator.of(context).pop();
                          manage();
                        },
                        child: const Text('Manage providers…'),
                      ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
