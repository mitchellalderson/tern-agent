import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../../state/documents.dart';
import '../../theme.dart';
import '../underline_tabs.dart';
import '../widgets.dart';
import 'safe_markdown.dart';

/// One open document: its text rendered (read-only), or its revisions.
class DocumentView extends StatefulWidget {
  const DocumentView({
    super.key,
    required this.docs,
    required this.id,
    this.onOpenLink,
  });

  final DocumentsState docs;
  final String id;
  final Future<void> Function(String url)? onOpenLink;

  @override
  State<DocumentView> createState() => _DocumentViewState();
}

class _DocumentViewState extends State<DocumentView> {
  var _history = false;

  @override
  Widget build(BuildContext context) {
    final docs = widget.docs;
    final c = context.colors;
    final content = docs.contentOf(widget.id);
    if (content == null) {
      return Center(
        child: docs.error != null
            ? Text(docs.error!, style: TextStyle(color: c.error))
            : const SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
      );
    }
    final meta = content.meta;
    final old = docs.viewing(widget.id);
    final revs = docs.revisionsOf(widget.id) ?? const [];
    final text = old?.content ?? content.content;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(18, 10, 12, 0),
          child: Row(
            children: [
              Expanded(
                child: UnderlineTabs(
                  spacing: 16,
                  tabs: [
                    UnderlineTab(
                      label: 'Document',
                      selected: !_history,
                      onTap: () => setState(() => _history = false),
                    ),
                    UnderlineTab(
                      label: 'History',
                      selected: _history,
                      onTap: () => setState(() => _history = true),
                    ),
                  ],
                ),
              ),
              QuietIconButton(
                icon: Icons.content_copy_outlined,
                tooltip: 'Copy text',
                onPressed: () => Clipboard.setData(ClipboardData(text: text)),
              ),
            ],
          ),
        ),
        const Hairline(),
        if (meta.tainted)
          _FromTheWeb(onReviewed: () => docs.markReviewed(widget.id)),
        if (old != null)
          _OlderBanner(
            rev: old.rev,
            onBack: () => docs.viewRevision(widget.id, null),
          ),
        Expanded(
          child: _history
              ? _History(
                  revisions: revs,
                  latest: meta.rev,
                  viewing: old?.rev,
                  onPick: (r) async {
                    await docs.viewRevision(widget.id, r.rev);
                    if (mounted) setState(() => _history = false);
                  },
                )
              : _Body(meta: meta, text: text, onOpenLink: widget.onOpenLink),
        ),
        const Hairline(),
        _StatusRow(
          meta: meta,
          revisions: revs,
          changedAt: docs.changedAt[meta.id],
        ),
      ],
    );
  }
}

class _Body extends StatelessWidget {
  const _Body({required this.meta, required this.text, this.onOpenLink});

  final DocumentMeta meta;
  final String text;
  final Future<void> Function(String url)? onOpenLink;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final Widget body = switch (meta.kind) {
      'markdown' => SafeMarkdown(text: text, onOpenLink: onOpenLink),
      'csv' when text.length <= maxRenderedBytes => _CsvTable(text: text),
      _ => SelectableText(text, style: mono.copyWith(color: c.text)),
    };
    return SelectionArea(
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(22, 18, 22, 28),
        child: Align(
          alignment: Alignment.topLeft,
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxWidth: meta.kind == 'markdown' ? 720 : double.infinity,
            ),
            child: body,
          ),
        ),
      ),
    );
  }
}

/// A comma-separated file as a grid. Quotes are honoured; at most 500 rows.
class _CsvTable extends StatelessWidget {
  const _CsvTable({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final rows = parseCsv(text);
    if (rows.isEmpty) return const Text('Empty');
    final shown = rows.take(501).toList();
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: Table(
        defaultColumnWidth: const IntrinsicColumnWidth(),
        border: TableBorder.all(color: c.hairline),
        children: [
          for (var i = 0; i < shown.length; i++)
            TableRow(
              decoration: i == 0 ? BoxDecoration(color: c.card) : null,
              children: [
                for (final cell in shown[i])
                  Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 8,
                      vertical: 5,
                    ),
                    child: Text(
                      cell,
                      style: TextStyle(
                        fontSize: 12.5,
                        fontWeight: i == 0 ? FontWeight.w600 : FontWeight.w400,
                      ),
                    ),
                  ),
              ],
            ),
        ],
      ),
    );
  }
}

/// Splits CSV text into rows of cells. Handles quoted cells, doubled quotes
/// and newlines inside quotes; pads short rows so a table can lay out.
List<List<String>> parseCsv(String text) {
  final rows = <List<String>>[];
  var row = <String>[];
  final cell = StringBuffer();
  var quoted = false;
  void endCell() {
    row.add(cell.toString());
    cell.clear();
  }

  void endRow() {
    endCell();
    if (!(row.length == 1 && row.first.isEmpty)) rows.add(row);
    row = <String>[];
  }

  for (var i = 0; i < text.length; i++) {
    final ch = text[i];
    if (quoted) {
      if (ch == '"') {
        if (i + 1 < text.length && text[i + 1] == '"') {
          cell.write('"');
          i++;
        } else {
          quoted = false;
        }
      } else {
        cell.write(ch);
      }
    } else if (ch == '"') {
      quoted = true;
    } else if (ch == ',') {
      endCell();
    } else if (ch == '\n') {
      endRow();
    } else if (ch != '\r') {
      cell.write(ch);
    }
  }
  if (cell.isNotEmpty || row.isNotEmpty) endRow();
  final width = rows.fold<int>(0, (m, r) => r.length > m ? r.length : m);
  for (final r in rows) {
    while (r.length < width) {
      r.add('');
    }
  }
  return rows;
}

class _FromTheWeb extends StatelessWidget {
  const _FromTheWeb({required this.onReviewed});

  final VoidCallback onReviewed;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      color: c.card,
      padding: const EdgeInsets.fromLTRB(18, 6, 8, 6),
      child: Row(
        children: [
          Icon(Icons.language, size: 15, color: c.muted),
          const SizedBox(width: 8),
          Expanded(
            child: Tooltip(
              message:
                  'Written from web or other untrusted content. Agents read '
                  'it back as data, not instructions.',
              child: Text(
                'From the web, not reviewed',
                style: TextStyle(color: c.muted, fontSize: 12.5),
              ),
            ),
          ),
          TextButton(
            onPressed: onReviewed,
            child: const Text('Mark as reviewed'),
          ),
        ],
      ),
    );
  }
}

class _OlderBanner extends StatelessWidget {
  const _OlderBanner({required this.rev, required this.onBack});

  final int rev;
  final VoidCallback onBack;

  @override
  Widget build(BuildContext context) => Container(
    color: context.colors.card,
    padding: const EdgeInsets.fromLTRB(18, 6, 8, 6),
    child: Row(
      children: [
        Expanded(
          child: Text(
            'Viewing revision $rev',
            style: TextStyle(color: context.colors.muted, fontSize: 12.5),
          ),
        ),
        TextButton(onPressed: onBack, child: const Text('Back to latest')),
      ],
    ),
  );
}

class _History extends StatelessWidget {
  const _History({
    required this.revisions,
    required this.latest,
    required this.viewing,
    required this.onPick,
  });

  final List<DocumentRevision> revisions;
  final int latest;
  final int? viewing;
  final void Function(DocumentRevision) onPick;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    if (revisions.isEmpty) {
      return Center(
        child: Text(
          'No revisions',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      );
    }
    return ListView(
      children: [
        for (final r in revisions)
          InkWell(
            onTap: () => onPick(r),
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 10),
              decoration: BoxDecoration(
                border: Border(bottom: BorderSide(color: c.hairline)),
              ),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(
                    width: 26,
                    child: Text(
                      '${r.rev}',
                      style: const TextStyle(fontWeight: FontWeight.w600),
                    ),
                  ),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(authorLabel(r)),
                        Text(
                          [
                            relativeTime(r.createdAt),
                            '+${r.added} −${r.removed}',
                            if (r.rebased) 'merged with newer text',
                            if (r.summary case final s? when s.isNotEmpty) s,
                          ].join(' · '),
                          style: TextStyle(color: c.muted, fontSize: 12),
                        ),
                      ],
                    ),
                  ),
                  if (r.rev == latest)
                    Text(
                      'current',
                      style: TextStyle(color: c.muted, fontSize: 12),
                    )
                  else if (r.rev == viewing)
                    Text(
                      'viewing',
                      style: TextStyle(color: c.accent, fontSize: 12),
                    ),
                ],
              ),
            ),
          ),
      ],
    );
  }
}

/// "research" for an agent, "You", "Edited outside tern", "tern".
String authorLabel(DocumentRevision r) => switch (r.authorKind) {
  'agent' => r.authorId,
  'user' => 'You',
  'external' => 'Edited outside tern',
  _ => 'tern',
};

class _StatusRow extends StatelessWidget {
  const _StatusRow({
    required this.meta,
    required this.revisions,
    this.changedAt,
  });

  final DocumentMeta meta;
  final List<DocumentRevision> revisions;
  final DateTime? changedAt;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final last = revisions.firstOrNull;
    final style = TextStyle(color: c.muted, fontSize: 12);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 8),
      child: Row(
        children: [
          Expanded(
            child: Text(
              [
                if (last != null)
                  'Changed by ${authorLabel(last)} · ${relativeTime(changedAt ?? last.createdAt)}',
                'revision ${meta.rev}',
              ].join(' · '),
              style: style,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          Flexible(
            child: Text(
              meta.path,
              style: style,
              overflow: TextOverflow.ellipsis,
            ),
          ),
        ],
      ),
    );
  }
}
