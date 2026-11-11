import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import java.io.*;
public class FlowVisionFocused extends GhidraScript {
 public void run() throws Exception {
  PrintWriter out=new PrintWriter(getScriptArgs()[0]);
  DecompInterface d=new DecompInterface(); d.openProgram(currentProgram);
  for(String a: new String[]{"100136024","100030eb8","10017d908","100134858","10012c2b0"}) {
   disassemble(toAddr(a)); Function f=getFunctionAt(toAddr(a)); if(f==null) f=createFunction(toAddr(a),null);
   out.println("FUNCTION "+a+" "+f.getName(true));
   DecompileResults r=d.decompileFunction(f,45,monitor);
   out.println(r.decompileCompleted()?r.getDecompiledFunction().getC():r.getErrorMessage()); out.flush();
  }
  out.close(); d.dispose();
 }
}
