import 'package:flutter/material.dart';

import '../../state/documents.dart';
import '../../theme.dart';
import '../underline_tabs.dart';
import '../widgets.dart';
import 'document_view.dart';

/// The right pane's two views: what the agent is doing, and its documents.
enum PaneMode { activity, documents }

/// The Activity / Documents switch at the top of the right pane. A dot marks
/// a document the agent changed while it was out of sight.
class PaneTabs extends StatelessWidget {
  const PaneTabs({
    super.key,
    required this.mode,
    required this.onMode,
    this.unseenDocuments = false,
  });

  final PaneMode mode;
  final ValueChanged<PaneMode> onMode;
  final bool unseenDocuments;

  @override
  Widget build(BuildContext context) => UnderlineTabs(
    spacing: 16,
    tabs: [
      UnderlineTab(
        label: 'Activity',
        fontSize: 14,
        selected: mode == PaneMode.activity,
        onTap: () => onMode(PaneMode.activity),
      ),
      UnderlineTab(
        label: 'Documents',
        fontSize: 14,
        selected: mode == PaneMode.documents,
        onTap: () => onMode(PaneMode.documents),
        leading: unseenDocuments
            ? Container(
                width: 6,
                height: 6,
                decoration: BoxDecoration(
                  color: context.colors.accent,
                  shape: BoxShape.circle,
                ),
              )
            : null,
      ),
    ],
  );
}

/// The Documents view of the right pane: one tab per open document, a menu of
/// every document, and the document in front (read-only).
class DocumentsPanel extends StatelessWidget {
  const DocumentsPanel({
    super.key,
    required this.docs,
    required this.mode,
    required this.onMode,
    required this.onClose,
    this.onOpenLink,
  });

  final DocumentsState docs;
  final PaneMode mode;
  final ValueChanged<PaneMode> onMode;
  final VoidCallback onClose;
  final Future<void> Function(String url)? onOpenLink;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return ListenableBuilder(
      listenable: docs,
      builder: (context, _) => Container(
        color: c.sidebar,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            WindowDragArea(
              child: Container(
                height: titleBarInset + 12,
                padding: const EdgeInsets.fromLTRB(18, 0, 8, 0),
                child: Row(
                  children: [
                    Expanded(
                      child: PaneTabs(
                        mode: mode,
                        onMode: onMode,
                        unseenDocuments: docs.unseen.isNotEmpty,
                      ),
                    ),
                    QuietIconButton(
                      icon: Icons.close,
                      tooltip: 'Close',
                      onPressed: onClose,
                    ),
                  ],
                ),
              ),
            ),
            const Hairline(),
            if (docs.open.isNotEmpty) _TabStrip(docs: docs),
            Expanded(
              child: docs.selected == null
                  ? _Library(docs: docs)
                  : DocumentView(
                      key: ValueKey(docs.selected),
                      docs: docs,
                      id: docs.selected!,
                      onOpenLink: onOpenLink,
                    ),
            ),
          ],
        ),
      ),
    );
  }
}

class _TabStrip extends StatelessWidget {
  const _TabStrip({required this.docs});

  final DocumentsState docs;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: c.hairline)),
      ),
      padding: const EdgeInsets.only(left: 12),
      child: Row(
        children: [
          Expanded(
            child: SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              child: Row(
                children: [
                  for (final id in docs.open)
                    Padding(
                      padding: const EdgeInsets.only(right: 4),
                      child: Row(
                        children: [
                          UnderlineTab(
                            label: docs.meta(id)?.title ?? 'Document',
                            selected: id == docs.selected,
                            onTap: () => docs.select(id),
                            leading: docs.unseen.contains(id)
                                ? Container(
                                    width: 6,
                                    height: 6,
                                    decoration: BoxDecoration(
                                      color: c.accent,
                                      shape: BoxShape.circle,
                                    ),
                                  )
                                : null,
                          ),
                          IconButton(
                            icon: const Icon(Icons.close, size: 13),
                            tooltip: 'Close tab',
                            color: c.muted,
                            visualDensity: VisualDensity.compact,
                            splashRadius: 12,
                            onPressed: () => docs.close(id),
                          ),
                        ],
                      ),
                    ),
                ],
              ),
            ),
          ),
          PopupMenuButton<String>(
            tooltip: 'All documents',
            icon: Icon(Icons.add, size: 18, color: c.muted),
            onSelected: docs.openDoc,
            itemBuilder: (_) => [
              for (final d in docs.all)
                PopupMenuItem(value: d.id, child: Text(d.title)),
              if (docs.all.isEmpty)
                const PopupMenuItem(
                  enabled: false,
                  child: Text('No documents'),
                ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Every document, newest first: the panel's front page when no tab is open.
class _Library extends StatelessWidget {
  const _Library({required this.docs});

  final DocumentsState docs;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    if (docs.all.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(24),
        child: Text(
          docs.loading
              ? 'Loading…'
              : 'No documents yet. Ask an agent to write one and it shows '
                    'up here.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      );
    }
    return ListView(
      children: [
        for (final d in docs.all)
          InkWell(
            onTap: () => docs.openDoc(d.id),
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 10),
              decoration: BoxDecoration(
                border: Border(bottom: BorderSide(color: c.hairline)),
              ),
              child: Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          d.title,
                          style: const TextStyle(fontWeight: FontWeight.w600),
                          overflow: TextOverflow.ellipsis,
                        ),
                        Text(
                          '${d.path} · ${relativeTime(d.updatedAt)}',
                          style: TextStyle(color: c.muted, fontSize: 12),
                          overflow: TextOverflow.ellipsis,
                        ),
                      ],
                    ),
                  ),
                  if (d.tainted)
                    Padding(
                      padding: const EdgeInsets.only(left: 8),
                      child: Tooltip(
                        message: 'From the web, not reviewed',
                        child: Icon(Icons.language, size: 15, color: c.muted),
                      ),
                    ),
                ],
              ),
            ),
          ),
      ],
    );
  }
}
