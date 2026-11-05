import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../theme.dart';

/// The starter shown for an empty-named template (the Blank one).
String starterName(AgentTemplate t) => t.name.isEmpty ? 'Blank' : t.name;

/// The create sheet's "Start from" select: one compact field that shows the
/// chosen starter, and opens [showStarterChooser] (type to filter, arrow
/// keys, Enter).
class StarterField extends StatelessWidget {
  const StarterField({
    super.key,
    required this.templates,
    required this.current,
    required this.onPicked,
  });

  final List<AgentTemplate> templates;
  final String? current;
  final ValueChanged<AgentTemplate> onPicked;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final t = templates.where((t) => t.id == current).firstOrNull;
    final level = t?.autonomyLevel;
    return InkWell(
      key: const Key('starter-select'),
      borderRadius: BorderRadius.circular(8),
      onTap: () async {
        final picked = await showStarterChooser(context, templates, current);
        if (picked != null) onPicked(picked);
      },
      child: InputDecorator(
        isEmpty: false,
        decoration: InputDecoration(
          labelText: 'Start from',
          isDense: true,
          border: OutlineInputBorder(borderRadius: BorderRadius.circular(8)),
          suffixIcon: Icon(Icons.unfold_more, size: 18, color: c.muted),
        ),
        child: Row(
          children: [
            Expanded(
              child: Text.rich(
                TextSpan(
                  children: [
                    TextSpan(
                      text: t == null ? 'Choose a starter' : starterName(t),
                    ),
                    if (t != null && t.description.isNotEmpty)
                      TextSpan(
                        text: '  ${t.description}',
                        style: TextStyle(color: c.muted, fontSize: 12),
                      ),
                  ],
                ),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(fontSize: 13),
              ),
            ),
            if (level != null) ...[
              const SizedBox(width: 8),
              Text(
                '${level.label} ceiling',
                key: const Key('starter-ceiling'),
                style: TextStyle(fontSize: 11.5, color: c.muted),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

Future<AgentTemplate?> showStarterChooser(
  BuildContext context,
  List<AgentTemplate> templates,
  String? current,
) => showDialog<AgentTemplate>(
  context: context,
  builder: (_) => _StarterChooser(templates: templates, current: current),
);

/// Search box over a list grouped by category (small headers); a row is the
/// name and a one-line description, with its ceiling at the right. The
/// Blank starter is always first.
class _StarterChooser extends StatefulWidget {
  const _StarterChooser({required this.templates, required this.current});

  final List<AgentTemplate> templates;
  final String? current;

  @override
  State<_StarterChooser> createState() => _StarterChooserState();
}

class _StarterChooserState extends State<_StarterChooser> {
  final _query = TextEditingController();
  final _rowKeys = <String, GlobalKey>{};
  var _hi = 0;

  /// What the list shows now: the Blank starter, then the matches by
  /// category. A category header is a String, a starter an AgentTemplate.
  List<Object> get _entries {
    final q = _query.text.trim().toLowerCase();
    bool match(AgentTemplate t) => q.isEmpty || t.searchText.contains(q);
    final blank = widget.templates.where((t) => t.name.isEmpty);
    final named = [
      for (final t in widget.templates)
        if (t.name.isNotEmpty && match(t)) t,
    ];
    final categories = <String>[];
    for (final t in named) {
      if (!categories.contains(t.category)) categories.add(t.category);
    }
    return [
      ...blank,
      for (final cat in categories) ...[
        if (cat.isNotEmpty) cat,
        ...named.where((t) => t.category == cat),
      ],
    ];
  }

  List<AgentTemplate> get _rows => _entries.whereType<AgentTemplate>().toList();

  @override
  void initState() {
    super.initState();
    final i = _rows.indexWhere((t) => t.id == widget.current);
    _hi = i < 0 ? 0 : i;
  }

  void _move(int delta) {
    final rows = _rows;
    if (rows.isEmpty) return;
    setState(() => _hi = (_hi + delta).clamp(0, rows.length - 1));
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final ctx = _rowKeys[rows[_hi].id]?.currentContext;
      if (ctx != null && ctx.mounted) {
        Scrollable.ensureVisible(
          ctx,
          alignmentPolicy: delta > 0
              ? ScrollPositionAlignmentPolicy.keepVisibleAtEnd
              : ScrollPositionAlignmentPolicy.keepVisibleAtStart,
        );
      }
    });
  }

  void _pick() {
    final rows = _rows;
    if (rows.isNotEmpty) {
      Navigator.of(context).pop(rows[_hi.clamp(0, rows.length - 1)]);
    }
  }

  @override
  void dispose() {
    _query.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final entries = _entries;
    final rows = entries.whereType<AgentTemplate>().toList();
    final hi = rows.isEmpty ? null : rows[_hi.clamp(0, rows.length - 1)].id;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(12)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460, maxHeight: 480),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 12, 12, 8),
              child: Focus(
                onKeyEvent: (_, e) {
                  if (e is KeyUpEvent) return KeyEventResult.ignored;
                  if (e.logicalKey == LogicalKeyboardKey.arrowDown) {
                    _move(1);
                    return KeyEventResult.handled;
                  }
                  if (e.logicalKey == LogicalKeyboardKey.arrowUp) {
                    _move(-1);
                    return KeyEventResult.handled;
                  }
                  return KeyEventResult.ignored;
                },
                child: TextField(
                  key: const Key('starter-search'),
                  controller: _query,
                  autofocus: true,
                  onChanged: (_) => setState(() {
                    // Typing aims at the first match; Blank stays on top.
                    final i = _rows.indexWhere((t) => t.name.isNotEmpty);
                    _hi = i < 0 ? 0 : i;
                  }),
                  onSubmitted: (_) => _pick(),
                  style: const TextStyle(fontSize: 13),
                  decoration: InputDecoration(
                    isDense: true,
                    hintText:
                        'Search ${widget.templates.where((t) => t.name.isNotEmpty).length} starters',
                    prefixIcon: const Icon(Icons.search, size: 18),
                    border: OutlineInputBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                  ),
                ),
              ),
            ),
            Flexible(
              child: SingleChildScrollView(
                padding: const EdgeInsets.fromLTRB(6, 0, 6, 6),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    if (rows.every((t) => t.name.isEmpty))
                      Padding(
                        padding: const EdgeInsets.all(16),
                        child: Text(
                          'No starter matches. Blank is always an option.',
                          style: TextStyle(fontSize: 12.5, color: c.muted),
                        ),
                      ),
                    for (final e in entries)
                      if (e is String)
                        Padding(
                          key: Key('starter-category-$e'),
                          padding: const EdgeInsets.fromLTRB(8, 10, 8, 2),
                          child: Text(
                            e,
                            style: TextStyle(
                              fontSize: 10.5,
                              fontWeight: FontWeight.w600,
                              color: c.muted,
                            ),
                          ),
                        )
                      else if (e is AgentTemplate)
                        _StarterRow(
                          key: _rowKeys.putIfAbsent(e.id, GlobalKey.new),
                          t: e,
                          highlighted: e.id == hi,
                          selected: e.id == widget.current,
                          onTap: () => Navigator.of(context).pop(e),
                        ),
                  ],
                ),
              ),
            ),
            Container(
              padding: const EdgeInsets.fromLTRB(14, 6, 14, 8),
              decoration: BoxDecoration(
                border: Border(top: BorderSide(color: c.hairline)),
              ),
              child: Text(
                'Type to filter · ↑↓ to move · Enter to pick · Esc to close',
                style: TextStyle(fontSize: 11, color: c.muted),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _StarterRow extends StatelessWidget {
  const _StarterRow({
    super.key,
    required this.t,
    required this.highlighted,
    required this.selected,
    required this.onTap,
  });

  final AgentTemplate t;
  final bool highlighted;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final level = t.autonomyLevel;
    return Tooltip(
      message: level == null
          ? t.description
          : '${t.description}\n${level.label} ceiling',
      waitDuration: const Duration(milliseconds: 600),
      child: InkWell(
        key: Key('starter-${t.id}'),
        borderRadius: BorderRadius.circular(8),
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
          decoration: BoxDecoration(
            color: highlighted ? c.selection : null,
            borderRadius: BorderRadius.circular(8),
          ),
          child: Row(
            children: [
              SizedBox(
                width: 18,
                child: selected
                    ? Icon(Icons.check, size: 15, color: c.accent)
                    : null,
              ),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      starterName(t),
                      style: const TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    if (t.description.isNotEmpty)
                      Text(
                        t.description,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(fontSize: 12, color: c.muted),
                      ),
                  ],
                ),
              ),
              if (level != null) ...[
                const SizedBox(width: 8),
                Text(
                  level.label,
                  style: TextStyle(fontSize: 11, color: c.muted),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}
