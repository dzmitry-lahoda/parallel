#!/usr/bin/env bash

DIR=$(cd -P -- "$(dirname -- "$0")" && pwd -P)

cd $DIR

set -xe

RELAY_WS_PORT=9949
RELAY_RPC_PORT=9939
RELAY_P2P_PORT=30339

PARA_WS_PORT=9948
PARA_RPC_PORT=9938
PARA_P2P_PORT=30338

PARA_ID=3350

PARA_CHAIN="${4:-heiko}"
RELAY_CHAIN="${5:-kusama}"
VOLUME="chains"
NODE_KEY="$1"
KEYSTORE_PATH="$2"
NODE_NAME="$3"

if [ $# -lt 3 ]; then
  echo "help: ./collator.sh <NODE_KEY> <KEYSTORE_PATH> <NODE_NAME>" && exit 1
fi

docker container stop $PARA_CHAIN-collator || true
docker container rm $PARA_CHAIN-collator || true

# docker volume rm $VOLUME || true

docker volume create $VOLUME || true

docker run --network=output_default --restart=always --name $PARA_CHAIN-collator \
  -d \
  -p $PARA_WS_PORT:$PARA_WS_PORT \
  -p $PARA_RPC_PORT:$PARA_RPC_PORT \
  -p $PARA_P2P_PORT:$PARA_P2P_PORT \
  -p $RELAY_WS_PORT:$RELAY_WS_PORT \
  -p $RELAY_RPC_PORT:$RELAY_RPC_PORT \
  -p $RELAY_P2P_PORT:$RELAY_P2P_PORT \
  -v "$VOLUME:/data" \
  -v "$(realpath $KEYSTORE_PATH):/app/keystore" \
  -v "/root/parallel/node/parallel/src/chain_spec/kerria-3350.json:/kerria-3350.json" \
  -v "/root/parallel/output/polkadot-local.json:/polkadot-local.json" \
  parallelfinance/parallel:kerria-3350 \
    -d /data \
    --chain="/kerria-3350.json" \
    --collator \
    --ws-port=$PARA_WS_PORT \
    --rpc-port=$PARA_RPC_PORT \
    --keystore-path=/app/keystore \
    --node-key=$NODE_KEY \
    --pruning archive \
    --wasm-execution=compiled \
    --execution=wasm \
    --ws-external \
    --rpc-external \
    --rpc-cors all \
    --rpc-methods Unsafe \
    --state-cache-size 0 \
    --listen-addr=/ip4/0.0.0.0/tcp/$PARA_P2P_PORT \
    --name=$NODE_NAME \
    --log='warn,cumulus-collator=trace,aura=trace,slots=trace' \
    --prometheus-external \
  -- \
    --chain="/polkadot-local.json" \
    --ws-port=$RELAY_WS_PORT \
    --rpc-port=$RELAY_RPC_PORT \
    --wasm-execution=compiled \
    --execution=wasm \
    --ws-external \
    --rpc-external \
    --rpc-cors all \
    --rpc-methods Unsafe \
    --database=RocksDb \
    --pruning=1000 \
    --listen-addr=/ip4/0.0.0.0/tcp/$RELAY_P2P_PORT \
    --name="${NODE_NAME}_Embedded_Relay" \
    --allow-private-ip \
    --bootnodes "/ip4/172.23.0.2/tcp/30333/p2p/12D3KooWJUdMQcvDf7Y6Un4SWMKWtiSHoD3vWXXHFM6t2uMX9ib8"

# --log='xcm=trace,sync=trace,aura=trace,sc_basic_authorship=trace,txpool=trace,sync=trace' \
docker logs -f $PARA_CHAIN-collator