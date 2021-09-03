pragma solidity ^0.8.0;


contract StakingRelayVerifier {
  event RelayAddressVerified(uint160 eth_addr, int8 workchain_id, uint256 addr_body);

  function verify_relay_staker_address(int8 workchain_id, uint256 address_body) external {
    emit RelayAddressVerified(uint160(msg.sender), workchain_id, address_body);
  }
}