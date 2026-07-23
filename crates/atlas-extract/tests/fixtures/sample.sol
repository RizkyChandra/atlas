// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "./Ownable.sol";

contract Base {
    function ping() public pure returns (uint) {
        return 1;
    }
}

contract Token is Base {
    uint public total;

    function mint(uint amount) public {
        total = total + amount;
        ping();
    }
}
