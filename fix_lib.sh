#!/bin/bash
sed -i '/<<<<<<< HEAD/,/=======/{
  /<<<<<<< HEAD/d
  /=======/d
}
/>>>>>>> origin\/main/d' contracts/receipt-anchor/src/lib.rs
