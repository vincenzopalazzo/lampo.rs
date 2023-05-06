import 'dart:convert';
import 'dart:ffi';
import 'dart:html';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

import 'package:lampo_dart/src/ffi/generated_bindings.dart';

final ffi = LampoFFI(DynamicLibrary.open("/usr/local/lib/liblampo_lib.so"));

/// Lampo Class Implementation
class Lampo {
  late Pointer<LampoDeamon> inner;
  ReceivePort? chan;

  Lampo({required String homePath}) {
    inner = ffi.new_lampod(homePath.toNativeUtf8().cast<Int8>());
  }

  Future<void> spawn() async {
    chan = ReceivePort();
    ffi.add_jsonrpc_on_unixsocket(inner);
    ffi.lampo_listen(inner);
    return Future.delayed(Duration(milliseconds: 10), () async {
        await Isolate.spawn((sendChan) {
        ffi.lampo_listen(inner);
        sendChan.send(true);
      },
      chan!.sendPort);
});
  }

  void stop() {
    chan!.listen((message) { });
    ffi.free_lampod(inner);
  }

  Future<Map<String, dynamic>> _call({required String method, Map<String, dynamic> payload = const {}}) async {
    var jsonStr = json.encode(payload);
    var response = ffi.lampod_call(inner, method.toNativeUtf8().cast(), jsonStr.toNativeUtf8().cast()).toString();
    return json.decode(response);
  }

  /// Dart is a language that it is used most for writing UI application,
  /// so we should design our solution across this, and avoid to
  /// run on the main tread all.
  ///
  /// In case you are running the application as command line, so there is no
  /// problem, and this will be has only benefits (maybe, if too many isolate are spawn?).
  Future<Map<String, dynamic>> call({required String method, Map<String, dynamic> payload = const {}}) async {
    return await Isolate.run(() => _call(method: method, payload: payload));
  }
}
