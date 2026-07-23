with Ada.Text_IO;

package body Sample is

   function Square (X : Integer) return Integer is
   begin
      return X * X;
   end Square;

   procedure Run is
      Y : Integer;
   begin
      Y := Square (5);
      Ada.Text_IO.Put_Line (Integer'Image (Y));
   end Run;

end Sample;
