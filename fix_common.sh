#!/bin/bash
sed -i '/<<<<<<< HEAD/,/=======/{
  /<<<<<<< HEAD/d
  /=======/d
}
/>>>>>>> origin\/main/d' contracts/common/src/lib.rs
