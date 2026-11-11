import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import java.io.*;

public class FlowVisionProbe extends GhidraScript {
 public void run() throws Exception {
  PrintWriter out = new PrintWriter(getScriptArgs()[0]);
  DecompInterface decomp = new DecompInterface(); decomp.openProgram(currentProgram);
  int count=0;
  for (Function f: currentProgram.getFunctionManager().getFunctions(true)) {
   String n=f.getName(true), lower=n.toLowerCase();
   if (lower.contains("pasteboardwriter") || lower.contains("draggingenter") || lower.contains("performdrag") || lower.contains("copy:") || lower.contains("copyaction") || (lower.contains("collectionview") && (lower.contains("itemfor") || lower.contains("writ") || lower.contains("drag")))) {
    out.println("FUNCTION " + f.getEntryPoint()+" "+n);
    DecompileResults r=decomp.decompileFunction(f,45,monitor);
    out.println(r.decompileCompleted()?r.getDecompiledFunction().getC():r.getErrorMessage());
    if(++count>=24) break;
   }
  }
  out.println("MATCHING_SYMBOLS");
  for(Symbol s: currentProgram.getSymbolTable().getAllSymbols(true)) {
   String n=s.getName(true); if(n.toLowerCase().matches(".*(pasteboard|dragging|thumbnail|filemodel|customcollection).*")) out.println(s.getAddress()+" "+n);
  }
  out.close(); decomp.dispose();
 }
}
