## 1 start collator
bash collator.sh "node-key" keystore kerria-collator kerria polkadot-local

## generate genesisHead
docker run --rm -v "/root/parallel/node/parallel/src/chain_spec/kerria-3350.json":/kerria-3350.json  parallelfinance/parallel:latest export-genesis-state --chain="/kerria-3350.json"

## On Relaychain
sudo.registrar.forceRegister
sudo.slots.forceLease

## On parachain
### check the key
rpc.author.haskey

## 注意区分spec的json和raw-json文件

### 当把宿主机的文件夹映射到docker内使，注意宿主机的文件夹权限，
### 外部rpc命令发送到docker容器后，可能不能往该文件夹内写私钥
### keystore目录下文件里的私钥需要加上双引号
### 注意如果keystore包含私钥文件，docker容器需要该私钥的读取权限
-v "$(realpath $KEYSTORE_PATH):/app/keystore" \
--keystore-path=/app/keystore \