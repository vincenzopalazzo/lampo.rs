import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

import 'package:lampo_dart/src/ffi/generated_bindings.dart';

final ffi = loadLibrary();

LampoFFI loadLibrary() {
  String? path;
  if (Platform.isAndroid) {
    path = "liblampo.so";
  } else if (Platform.isLinux) {
    path = "/usr/local/lib/liblampo.so";
  } else {
    throw Exception("platform not supported");
  }
  return LampoFFI(DynamicLibrary.open(path));
}

/// Lampo Class Implementation
class Lampo {
  late Pointer<LampoDeamon> inner;

  Lampo({required String homePath}) {
    inner = ffi.new_lampod(homePath.toNativeUtf8().cast<Int8>());
  }

  void spawn() {
    ffi.add_jsonrpc_on_unixsocket(inner);
    ffi.lampo_listen(inner);
  }

  void stop() {
    ffi.free_lampod(inner);
  }

  Future<Map<String, dynamic>> _call(
      {required String method, Map<String, dynamic> payload = const {}}) async {
    var jsonStr = json.encode(payload);
    var response = ffi
        .lampod_call(
            inner, method.toNativeUtf8().cast(), jsonStr.toNativeUtf8().cast())
        .cast<Utf8>()
        .toDartString();
    return json.decode(response);
  }

  /// Dart is a language that it is used most for writing UI application,
  /// so we should design our solution across this, and avoid to
  /// run on the main tread all.
  ///
  /// In case you are running the application as command line, so there is no
  /// problem, and this will be has only benefits (maybe, if too many isolate are spawn?).
  Future<Map<String, dynamic>> call(
      {required String method, Map<String, dynamic> payload = const {}}) async {
    return await _call(method: method, payload: payload);
  }
}
