let
  pkgs = import <nixpkgs> { };
  greet = name: "hello ${name}";
  message = greet "world";
in
{
  inherit message;
  tool = pkgs.hello;
}
