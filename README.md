<div align="center">
  <h1>Lampo SDK</h1>

  <img src="https://github.com/saradurante/lampo.docs/blob/dc0dce971c3052f0e9dd668fdf0c7376b12fee7b/imgs/web/icon-512.png?raw=true"  width="150" height="150" />


  <p>
    <strong>Fast and modular lightning network implementation for all usages, written in Rust.</strong>
  </p>

  <h4>
    <a href="https://lampo.mintlify.app/">Project Homepage</a>
  </h4>
</div>

This repository contains a set of crates that are useful for working with core lightning in the Rust programming environment.

## Crates

These are the complete list of crates supported right now:

| Crate       | Description                                   | Version     |
|:------------|:---------------------------------------------:|------------:|
| lampod-cli  | Lampo Daemon command line interface to run the daemon | _unrelated_ |
| lampo-cli   | Simple Lampo command line interface to interact with the daemon | _unrelated_ |

## How to Install

To install all the requirements binary we need to 
have rust installed, and then run the following command

```
make install
```

After you have `lampod-cli` and `lampo-cli` available and the following
commands can be ran

```
➜  ~ lampod-cli --config /tmp/.lampo --client nakamoto
2023-06-23T21:32:23.276Z INFO lampod::chain::wallet mnemonic works `maple have fitness decide food joy flame coast stereo front grab stumble`
2023-06-23T21:32:23.278Z INFO lampod::chain::wallet descriptor: wpkh([7622557d/84'/1'/0']tpubDC98fnPtvTdNiuUEHx9ZN4kD7WyZ9XmjvsDVPpG1rQWAGP7xpkxcjLvr8ZqJqC1h2zynykVmtuPo1kzyhQuWtc9eN9sLUDghhrnDipNhpwV/1/*)#cj0v9e6w
✓ Wallet Generated, please store this works in a safe way
 waller-keys  maple have fitness decide food joy flame coast stereo front grab stumble
2023-06-23T21:32:23.280Z INFO client Initializing client (Testnet)..
2023-06-23T21:32:23.280Z INFO client Genesis block hash is 000000000933ea01ad0ee984209779baaec3ced90fa3f408719526f8d77f4943
2023-06-23T21:32:23.280Z INFO client Found existing store "/home/vincent/.nakamoto/testnet/headers.db"
2023-06-23T21:32:23.280Z INFO client Store height = 2438853
2023-06-23T21:32:23.280Z INFO client Loading block headers from store..
2023-06-23T21:32:23.281Z INFO lampod::chain::wallet bdk stert to sync
2023-06-23T21:32:26.261Z INFO client Initializing block filters..
2023-06-23T21:32:26.261Z INFO client Found existing store "/home/vincent/.nakamoto/testnet/filters.db"
2023-06-23T21:32:26.262Z INFO client Filters height = 2429165
2023-06-23T21:32:26.262Z INFO client Loading filter headers from store..

```

N.B: Store your wallet works, and then reuse them to restore the waller with `--restore-wallet`.

Please note that this need to have a `lampo.conf` in the path `/tmp/.lampo`.

Then you can query the node with the following command 

``` 
➜  ~ lampo-cli --socket /tmp/.lampo/lampod.socket getinfo
{
  "node_id": "035b889551a44e502cd0cd6657acf067336034986cd6639b222cd4be563a7fc205",
  "peers": 0,
  "channels": 0
}
```

### To run integration tests with core lightning:

Make sure you have compiled core-lightning in developer mode. The installation guide can be found [here](https://docs.corelightning.org/docs/installation).

Integration tests can be run using the following command

```
make integration
```

## Contributing guidelines

Please read our [Hacking guide](/docs/MAINTAINERS.md).
