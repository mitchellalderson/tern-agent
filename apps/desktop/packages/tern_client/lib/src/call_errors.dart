/// A failed model call in plain words (docs/provider-setup-spec.md §12.4).
/// The same wording as the core's `friendly_call_error`.
library;

class FriendlyError {
  const FriendlyError({
    required this.code,
    required this.message,
    required this.fixInModels,
  });

  /// `not_found`, `unauthorized`, `forbidden`, `payment`, `rate_limited`,
  /// `server` or `unreachable`.
  final String code;

  /// A sentence a person can act on.
  final String message;

  /// The fix is in the provider's settings (key, URL or model name), so the
  /// UI can offer "Fix in Models".
  final bool fixInModels;
}

/// Explain a model-call error such as `HTTP 404: {"error":...}` or
/// `network error: ...` for the provider called [label] (e.g. "OpenRouter").
/// Null when the text is not one of those; show it as is. Keep the raw text
/// as the details.
FriendlyError? friendlyCallError(String raw, String label) {
  final text = raw.trim();
  final http = RegExp(
    r'^HTTP (\d{3})\s*:?(.*)$',
    dotAll: true,
  ).firstMatch(text);
  if (http != null) {
    final status = int.parse(http.group(1)!);
    final body = (http.group(2) ?? '').toLowerCase();
    FriendlyError f(String code, String message, bool fix) =>
        FriendlyError(code: code, message: message, fixInModels: fix);
    switch (status) {
      case 401:
        return f(
          'unauthorized',
          '$label rejected the API key. Check the key in Models.',
          true,
        );
      case 402:
        return f(
          'payment',
          '$label says the account is out of credit. Add credit there, then try again.',
          false,
        );
      case 403:
        return f(
          'forbidden',
          "$label won't let this key use that. Check the key's permissions and the model name.",
          true,
        );
      case 404:
        final model =
            body.contains('model') || body.contains('no endpoints found');
        return f(
          'not_found',
          model
              ? "$label couldn't find that model. Check the model name."
              : "$label couldn't find that endpoint or model. Check the model name.",
          true,
        );
      case 429:
        return f(
          'rate_limited',
          '$label is limiting requests right now. Wait a moment and try again.',
          false,
        );
    }
    if (status >= 500 && status <= 599) {
      return f(
        'server',
        '$label had a problem on its side (HTTP $status). Try again shortly.',
        false,
      );
    }
    return null;
  }
  if (text.startsWith('network error:')) {
    return FriendlyError(
      code: 'unreachable',
      message:
          "Couldn't reach $label. Check your connection, or the server address in Models.",
      fixInModels: true,
    );
  }
  return null;
}
