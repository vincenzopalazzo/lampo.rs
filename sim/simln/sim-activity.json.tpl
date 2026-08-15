{
  "nodes": [
    { "address": "127.0.0.1:3543", "api_key": "<hex lk3>", "cert": "/home/vincenzopalazzo/ldk-server/ldk-nodes/lk3/data/tls.crt" },
    { "address": "127.0.0.1:3541", "api_key": "<hex lk1>", "cert": "/home/vincenzopalazzo/ldk-server/ldk-nodes/lk1/data/tls.crt" }
  ],
  "activity": [
    {
      "source": "127.0.0.1:3543",
      "destination": "<pubkey of lk1>",
      "interval_secs": 60,
      "amount": { "min_msat": 10000, "max_msat": 5000000 }
    }
  ]
}
