sudo route delete default
sudo route add default 10.10.10.1

sudo route delete default
sudo route add default 192.168.31.1

route -n add -net 123.150.76.218 10.10.10.2/24 10.10.10.1
route -n add -net 113.108.81.189 -netmask 255.255.255.0 10.10.10.1

sudo route add -net 198.18.0.0/16 10.10.10.2
sudo route delete -net 198.18.0.0/16 10.10.10.2

sudo route add -net 106.75.216.201/32 10.10.10.2
sudo route delete -net 106.75.216.201/32 10.10.10.2

sudo route add -net 119.28.5.105/32 10.10.10.2
sudo route delete -net 119.28.5.105/32 10.10.10.2

sudo route add -inet6 2400:8905::f03c:94ff:fe1c:a95e fe80::52ed:3cff:fe18:2e99%utun8
sudo route delete -inet6 2400:8905::f03c:94ff:fe1c:a95e fe80::52ed:3cff:fe18:2e99%utun8

sudo route add -net 2409:8c28:203:33:60::3d 10.10.10.2
sudo route delete -net 2409:8c28:203:33:60::3d 10.10.10.2