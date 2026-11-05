/// Fallback chains (T6a): a primary model and the models tried, in order,
/// when it errors, is overloaded or rate-limited, or its key is rejected.
///
/// A chain is stored as one `provider:model` string or a list of them
/// (`default_model`, `subagents.models`, an agent's `model`). These helpers
/// are the one place the app turns the visual editor's list into that, and
/// checks it before the daemon does.
library;

import 'client.dart';
import 'providers.dart';

/// `provider:id` split at the first colon, or null when it isn't one.
/// Model ids may contain colons themselves (`ollama:llama3:8b`).
({String provider, String id})? parseModelRef(String s) {
  final text = s.trim();
  final colon = text.indexOf(':');
  if (colon <= 0 || colon == text.length - 1) return null;
  return (provider: text.substring(0, colon), id: text.substring(colon + 1));
}

/// Why saving a chain failed, in a sentence a person can act on. [e] is
/// what `PUT /v1/config/settings/default_model` (or an agent update) threw.
String chainSaveMessage(TernException e) {
  if (e.status == 403) {
    return "This device isn't allowed to change models. Ask the owner of "
        'this tern, or change it on the server.';
  }
  if (e.unreachable) {
    return "Couldn't reach tern to save that. Nothing was changed; try again "
        'in a moment.';
  }
  final m = e.message;
  if (m.contains('expected provider:model')) {
    return 'Each model must look like provider:model, for example '
        'anthropic:claude-sonnet-4-5.';
  }
  if (m.contains('at least one entry')) {
    return 'A list needs at least one model.';
  }
  return m;
}

/// A chain as the API takes it: null (none), one model, or a list.
Object? chainToWire(List<String> chain) => switch (chain) {
  [] => null,
  [final one] => one,
  _ => chain,
};

/// Why an entry of a chain can't be used (or may not work).
enum ChainIssueKind {
  /// Not `provider:model`.
  badFormat,

  /// No connected provider has that name.
  unknownProvider,

  /// The provider is there but doesn't list that model right now.
  notListed,

  /// An earlier entry is the same model, so this one would never run.
  duplicate,
}

class ChainIssue {
  const ChainIssue(this.index, this.model, this.kind, this.message);

  final int index;
  final String model;
  final ChainIssueKind kind;

  /// A sentence a person can act on.
  final String message;

  /// Saving should wait until it's fixed; [ChainIssueKind.notListed] only
  /// warns (a typed id can be right though the provider's list lacks it).
  bool get blocking => kind != ChainIssueKind.notListed;
}

/// What is wrong with `chain`, entry by entry, given the providers' model
/// lists (`/v1/models`). With no lists yet (not loaded, or a daemon too old
/// to list) only the format and duplicates are checked.
List<ChainIssue> checkModelChain(
  List<String> chain, {
  List<ProviderModels>? lists,
}) {
  final issues = <ChainIssue>[];
  final seen = <String>{};
  for (var i = 0; i < chain.length; i++) {
    final m = chain[i];
    final ref = parseModelRef(m);
    if (ref == null) {
      issues.add(
        ChainIssue(
          i,
          m,
          ChainIssueKind.badFormat,
          'Write it as provider:model, for example '
          'anthropic:claude-sonnet-4-5.',
        ),
      );
      continue;
    }
    if (!seen.add(m.trim())) {
      issues.add(
        ChainIssue(
          i,
          m,
          ChainIssueKind.duplicate,
          'This model is already earlier in the list, so this copy would '
          'never run.',
        ),
      );
      continue;
    }
    if (lists == null || lists.isEmpty) continue;
    final provider = lists.where((l) => l.provider == ref.provider).firstOrNull;
    if (provider == null) {
      issues.add(
        ChainIssue(
          i,
          m,
          ChainIssueKind.unknownProvider,
          'No connected provider is called "${ref.provider}". Connect it in '
          'Models, or pick a model from a provider you have.',
        ),
      );
    } else if (provider.models.isNotEmpty &&
        !provider.models.any((x) => x.id == ref.id)) {
      issues.add(
        ChainIssue(
          i,
          m,
          ChainIssueKind.notListed,
          "${provider.label} doesn't list this model right now. It can still "
          'work if the id is exactly right.',
        ),
      );
    }
  }
  return issues;
}
